//! Compile-time execution of user macros and consts.
//!
//! [`IsolateMacroHost`] implements [`compiler::macros::MacroHost`]: each expansion
//! entry runs with only pure host natives wired (see
//! [`compiler::macros::native_allowed_at_compile_time`]), a step budget and
//! a heap cap ([`ComptimeLimits`], `--comptime-steps` / `--comptime-mem`),
//! so a macro cannot touch the outside world, hang the build or exhaust its
//! memory.
//!
//! A program's MIR runs on the interpreter (`coil-mir-interp`), over an
//! [`Isolate`] that holds what a run needs (natives, statics, the heap).
//!
//! Each thread keeps one isolate per expansion program, with the natives and
//! the program loaded once (#1008). After a call that returns, the isolate
//! is reset ([`Isolate::reset`]: statics and heap back to their initial
//! state), so every call starts as on a fresh one. A call that panics, runs
//! out of budget or aborts drops the isolate; the next call builds a new one.

mod io_gate;

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

use common::Value;
use compiler::ComptimeLimits;
use compiler::macros::{
    CompiledExpansion, ComptimeIo, ConstEval, ConstEvalError, ConstEvalOutcome, ConstRecord, FoldCall, FoldValue, MacroHost,
    MacroRun, native_allowed_at_compile_time,
};
use machine::native::NativeCode;
use machine::{FfiError, HostClosureFn, Isolate, IsolateProgram, NativeFn, Object, SnapRoot};

/// Runs macros and consts in a sandbox.
#[derive(Default)]
pub struct IsolateMacroHost;

impl IsolateMacroHost {
    pub fn shared() -> Arc<dyn MacroHost> {
        Arc::new(Self)
    }
}

/// Make every pipeline created after this call run user macros on an isolate.
pub fn install() {
    compiler::macros::install_default_host(IsolateMacroHost::shared());
}

impl MacroHost for IsolateMacroHost {
    fn build_id(&self) -> &'static str {
        coil_build_id::BUILD_ID
    }

    fn runs_mir(&self) -> bool {
        true
    }

    fn run(
        &self,
        program: &CompiledExpansion,
        calls: &[(u32, String)],
        limits: &ComptimeLimits,
        io: &ComptimeIo,
    ) -> Vec<MacroRun> {
        let outer = io_gate::set_active(Some(Arc::new(io.clone())));
        let out = calls
            .iter()
            .map(|(entry, input)| {
                io_gate::start_recording();
                io_gate::start_clock(limits.timeout_secs);
                let result = run_entry(program, *entry, input, limits)
                    .and_then(|out| io_gate::clock_ran_out().map_or(Ok(out), Err));
                (result, io_gate::take_recording())
            })
            .collect();
        io_gate::set_active(outer);
        out
    }

    fn eval_consts(
        &self,
        program: &CompiledExpansion,
        consts: &[ConstEval],
        folds: &[FoldCall],
        limits: &ComptimeLimits,
        io: &[ComptimeIo],
    ) -> Result<ConstEvalOutcome, ConstEvalError> {
        let captured = Captured::default();
        let outer = machine::io::set_output_redirect(None);
        let kinds = machine::swap_static_word_kinds(program.static_word_kinds.clone());
        let io: Vec<Arc<ComptimeIo>> = io.iter().cloned().map(Arc::new).collect();
        let result = eval_in(program, consts, folds, limits, &io, &captured);
        io_gate::set_active(None);
        machine::swap_static_word_kinds(kinds);
        machine::io::set_output_redirect(outer);
        result
    }
}

fn const_isolate(program: &CompiledExpansion, captured: &Captured) -> Box<Isolate> {
    let mut isolate = new_isolate(program);
    isolate.heap_mut()
        .set_class_word_kinds(machine::class_kind_table(&program.class_word_kinds));
    let _ = isolate.with_output(captured.clone());
    isolate
}

fn snap_roots(isolate: &Isolate, consts: &[ConstEval], done: &[usize]) -> Vec<SnapRoot> {
    done.iter()
        .map(|&i| SnapRoot {
            slot: consts[i].slot,
            value: isolate.static_value(consts[i].slot).unwrap_or_default(),
            kind: consts[i].kind,
        })
        .collect()
}

/// Run each initializer and keep its value in its slot, then snapshot them
/// all, then run the folds. A silent item or fold that fails leaves the
/// isolate in an unknown state, so the values computed so far move to a new one
/// through a snapshot.
fn eval_in(
    program: &CompiledExpansion,
    consts: &[ConstEval],
    folds: &[FoldCall],
    limits: &ComptimeLimits,
    io: &[Arc<ComptimeIo>],
    captured: &Captured,
) -> Result<ConstEvalOutcome, ConstEvalError> {
    let mut isolate = const_isolate(program, captured);
    let mut done: Vec<usize> = Vec::new();
    let mut skipped: Vec<usize> = Vec::new();
    let mut silent_left = ComptimeLimits::SILENT_MODULE_STEPS;
    let mut records = Vec::new();
    for (index, c) in consts.iter().enumerate() {
        if let Some(snapshot) = &c.cached {
            for (slot, value) in isolate.heap_mut().restore_snapshot(snapshot) {
                isolate.set_static(slot, value);
            }
            done.push(index);
            continue;
        }
        let item_limits = if c.silent {
            if silent_left == 0 {
                skipped.push(index);
                continue;
            }
            ComptimeLimits {
                steps: ComptimeLimits::SILENT_CALL_STEPS.min(silent_left),
                ..*limits
            }
        } else {
            *limits
        };
        captured.0.lock().expect("capture lock").clear();
        io_gate::set_active(if c.silent { None } else { io.get(c.io as usize).cloned() });
        if c.record {
            io_gate::start_recording();
        }
        io_gate::start_clock(limits.timeout_secs);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            call_limited(&mut isolate, c.entry, &[], &item_limits)
        }));
        io_gate::set_active(None);
        let accesses = io_gate::take_recording();
        if c.silent {
            silent_left = silent_left.saturating_sub(isolate.steps_charged());
        }
        // Past the clock, a result is an evaluation cut short.
        let outcome = match outcome {
            Ok(Ok(_)) if !c.silent && let Some(msg) = io_gate::clock_ran_out() => Ok(Err(msg)),
            other => other,
        };
        match outcome {
            Ok(Ok(value)) => {
                isolate.set_static(c.slot, value);
                done.push(index);
                if !accesses.is_empty() {
                    let root = SnapRoot { slot: c.slot, value, kind: c.kind };
                    // A value with no snapshot form fails the whole capture below.
                    if let Ok(snapshot) = isolate.heap().capture_snapshot(&[root]) {
                        records.push(ConstRecord { index, accesses, snapshot });
                    }
                }
            }
            Ok(Err(_)) | Err(_) if c.silent => {
                skipped.push(index);
                isolate = rebuild(program, consts, &mut done, &mut skipped, isolate, captured)?;
            }
            Ok(Err(reason)) => {
                let _ = isolate.restore_output();
                return Err(ConstEvalError::Failed {
                    index,
                    reason: failure_reason(reason, captured),
                });
            }
            Err(_) => {
                return Err(ConstEvalError::Failed {
                    index,
                    reason: failure_reason("it aborted".to_string(), captured),
                });
            }
        }
    }
    let snapshot = capture(&isolate, consts, &mut done, &mut skipped)?;
    skipped.sort_unstable();
    let mut folded = Vec::with_capacity(folds.len());
    let mut over_budget = Vec::new();
    for (index, f) in folds.iter().enumerate() {
        if silent_left == 0 {
            folded.push(None);
            continue;
        }
        let item_limits = ComptimeLimits {
            steps: ComptimeLimits::SILENT_CALL_STEPS.min(silent_left),
            ..*limits
        };
        let started = std::time::Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let args: Vec<Value> = f
                .args
                .iter()
                .map(|a| match a {
                    FoldValue::Word(w) => Value::from(*w),
                    FoldValue::Text(s) => {
                        let s = isolate.heap_mut().intern(s.clone());
                        Value::from(s.as_ptr() as *mut u8 as u64)
                    }
                })
                .collect();
            let ret = call_limited(&mut isolate, f.entry, &args, &item_limits)?;
            if !f.text {
                return Ok(FoldValue::Word(ret.bits()));
            }
            match isolate.heap().find_object_by_addr(ret.bits()) {
                Some(Object::String(s)) => Ok(FoldValue::Text(s.as_ref().data.to_string())),
                _ => Err("it did not return a string".to_string()),
            }
        }));
        silent_left = silent_left.saturating_sub(isolate.steps_charged());
        if isolate.step_budget_exhausted() {
            over_budget.push((index, started.elapsed().as_micros() as u64));
        }
        match outcome {
            Ok(Ok(value)) => folded.push(Some(value)),
            _ => {
                folded.push(None);
                let _ = isolate.restore_output();
                isolate = restored_isolate(program, &snapshot, captured);
            }
        }
    }
    let _ = isolate.restore_output();
    Ok(ConstEvalOutcome {
        snapshot,
        skipped,
        folded,
        over_budget,
        records,
    })
}

/// A new isolate whose statics hold `snapshot`'s values.
fn restored_isolate(program: &CompiledExpansion, snapshot: &common::HeapSnapshot, captured: &Captured) -> Box<Isolate> {
    let mut isolate = const_isolate(program, captured);
    for (slot, value) in isolate.heap_mut().restore_snapshot(snapshot) {
        isolate.set_static(slot, value);
    }
    isolate
}

/// Snapshot the `done` items' values; a silent or `startup_ok` item whose
/// value has no snapshot form moves to `skipped`.
fn capture(
    isolate: &Isolate,
    consts: &[ConstEval],
    done: &mut Vec<usize>,
    skipped: &mut Vec<usize>,
) -> Result<common::HeapSnapshot, ConstEvalError> {
    loop {
        match isolate.heap().capture_snapshot(&snap_roots(isolate, consts, done)) {
            Ok(snapshot) => return Ok(snapshot),
            Err(e) if consts[done[e.root]].silent || consts[done[e.root]].startup_ok => skipped.push(done.remove(e.root)),
            Err(mut e) => {
                e.root = done[e.root];
                return Err(ConstEvalError::Unsnapshottable(e));
            }
        }
    }
}

/// A new isolate holding the values computed so far.
fn rebuild(
    program: &CompiledExpansion,
    consts: &[ConstEval],
    done: &mut Vec<usize>,
    skipped: &mut Vec<usize>,
    old: Box<Isolate>,
    captured: &Captured,
) -> Result<Box<Isolate>, ConstEvalError> {
    let snapshot = capture(&old, consts, done, skipped);
    let mut old = old;
    let _ = old.restore_output();
    drop(old);
    Ok(restored_isolate(program, &snapshot?, captured))
}

/// The failure text: the run's own reason, else what the call printed.
/// What the run printed before it stopped. A native the host refused says
/// what to grant, and where in a library it was called does not help.
fn printed_text(captured: &Captured) -> String {
    let printed = String::from_utf8_lossy(&captured.0.lock().expect("capture lock")).trim().to_string();
    let (first, rest) = printed.split_once('\n').unwrap_or((&printed, ""));
    match first.strip_prefix("panic: ") {
        Some(msg) if msg.contains(" while compiling") || msg.contains("`--comptime-") => {
            let msg = msg.rsplit_once(" at ").map_or(msg, |(m, _)| m);
            if rest.is_empty() { msg.to_string() } else { format!("{msg}\n{rest}") }
        }
        _ => printed,
    }
}

fn failure_reason(reason: String, captured: &Captured) -> String {
    let printed = printed_text(captured);
    match reason {
        r if r.is_empty() && !printed.is_empty() => printed,
        r if r.is_empty() => "it panicked".to_string(),
        r if r == "it aborted" && !printed.is_empty() => printed,
        r => r,
    }
}

/// A thread's isolate for one expansion program. Holding the MIR `Arc`
/// keeps the program alive, so its address never names another one.
struct WarmIsolate {
    program: Arc<Vec<compiler::native_mir::NativeBody>>,
    isolate: Box<Isolate>,
}

thread_local! {
    static WARM: RefCell<Vec<WarmIsolate>> = const { RefCell::new(Vec::new()) };
}

/// This thread's isolate for `program`, or a new one with it loaded.
fn take_isolate(program: &CompiledExpansion) -> Box<Isolate> {
    let warm = WARM.with(|warm| {
        let mut warm = warm.borrow_mut();
        let at = warm.iter().position(|w| Arc::ptr_eq(&w.program, &program.mir))?;
        Some(warm.swap_remove(at).isolate)
    });
    warm.unwrap_or_else(|| new_isolate(program))
}

fn keep_isolate(program: &CompiledExpansion, isolate: Box<Isolate>) {
    WARM.with(|warm| {
        warm.borrow_mut().push(WarmIsolate { program: program.mir.clone(), isolate });
    });
}

fn new_isolate(program: &CompiledExpansion) -> Box<Isolate> {
    let mut isolate = Box::new(Isolate::new(IsolateProgram {
        constants: program.constants.clone(),
        strings: program.strings.clone(),
        debug: program.program_debug.clone(),
        native: interpreted(program),
        static_slots: program.static_slot_count,
    }));
    for native in sandbox_natives() {
        isolate.register_native(native.clone());
    }
    isolate
}

/// `program`'s MIR made runnable, once for every thread.
fn interpreted(program: &CompiledExpansion) -> Arc<NativeCode> {
    let code = program.runnable.get_or_init(|| {
        let pcs = program.program_debug.fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
        let prepared = coil_mir_interp::prepare(program.mir.to_vec(), &pcs);
        let complete = prepared.refused.is_empty();
        let code = NativeCode::new(prepared.entries, prepared.sites).with_complete(complete).with_keep(prepared.keep);
        Arc::new(code)
    });
    Arc::clone(code).downcast::<NativeCode>().expect("this host made it")
}

/// Standard host table with every impure native replaced by a stub that
/// fails the macro. Ids stay in ABI order. Built once per process.
fn sandbox_natives() -> &'static [Arc<dyn NativeFn>] {
    static NATIVES: OnceLock<Vec<Arc<dyn NativeFn>>> = OnceLock::new();
    NATIVES.get_or_init(build_sandbox_natives)
}

fn build_sandbox_natives() -> Vec<Arc<dyn NativeFn>> {
    let mut names = Vec::new();
    let natives = machine::build_standard_host_natives(|name, _| names.push(name.to_string()));
    natives
        .into_iter()
        .zip(names)
        .map(|(native, name)| {
            if native_allowed_at_compile_time(&name) {
                native
            } else if name == common::THREAD_SPAWN_SHARED_NATIVE {
                // Only auto-par calls it, and a failed spawn is its way out.
                machine::host_natives::refused_spawn_shared()
            } else if let Some(gated) = io_gate::gated(&name, native.clone()) {
                gated
            } else {
                let sig = native.signature().clone();
                Arc::new(HostClosureFn::new(sig, move |_heap, _args| {
                    Err(FfiError::Panic(format!(
                        "`{name}` is not available while compiling"
                    )))
                })) as Arc<dyn NativeFn>
            }
        })
        .collect()
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_entry(program: &CompiledExpansion, entry: u32, input: &str, limits: &ComptimeLimits) -> Result<String, String> {
    let mut isolate = take_isolate(program);
    let captured = Captured::default();
    // `with_output` installs a thread-local redirect pointing into the
    // isolate; put the caller's back before it is dropped.
    let outer = machine::io::set_output_redirect(None);
    let kinds = machine::swap_static_word_kinds(program.static_word_kinds.clone());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = isolate.with_output(captured.clone());
        let result = call_entry(&mut isolate, entry, input, limits);
        let _ = isolate.restore_output();
        result
    }));
    machine::swap_static_word_kinds(kinds);
    machine::io::set_output_redirect(outer);
    // Only a call that returned leaves the isolate reusable; any other
    // outcome drops it.
    if matches!(outcome, Ok(Ok(_))) {
        isolate.reset();
        keep_isolate(program, isolate);
    }
    let printed = printed_text(&captured);
    match outcome {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(reason)) if reason.is_empty() => Err(if printed.is_empty() {
            "it panicked".to_string()
        } else {
            printed
        }),
        Ok(Err(reason)) => Err(reason),
        Err(_) => Err(if printed.is_empty() {
            "it aborted".to_string()
        } else {
            printed
        }),
    }
}

fn call_entry(isolate: &mut Isolate, entry: u32, input: &str, limits: &ComptimeLimits) -> Result<String, String> {
    let input = isolate.heap_mut().intern(input.to_string());
    let arg = Value::from(input.as_ptr() as *mut u8 as u64);
    let ret = call_limited(isolate, entry, &[arg], limits)?;
    match isolate.heap().find_object_by_addr(ret.bits()) {
        Some(Object::String(s)) => Ok(s.as_ref().data.to_string()),
        _ => Err("it did not return a string".to_string()),
    }
}

/// Call `entry` within `limits`. An empty error is a plain panic (its
/// message went to the run's output).
fn call_limited(isolate: &mut Isolate, entry: u32, args: &[Value], limits: &ComptimeLimits) -> Result<Value, String> {
    isolate.set_step_budget(Some(limits.steps));
    isolate.set_heap_cap(Some(usize::try_from(limits.heap_bytes).unwrap_or(usize::MAX)));
    let Some(ret) = isolate.call(entry, args) else {
        return Err("the MIR interpreter cannot run a body it calls".to_string());
    };
    if isolate.step_budget_exhausted() {
        return Err(format!(
            "it ran too long: {} steps (loop iterations + calls) is the limit; \
             is there an infinite loop? Raise it with `--comptime-steps N`",
            limits.steps
        ));
    }
    if isolate.heap_cap_exceeded() {
        return Err(format!(
            "it used too much memory: {} MiB of live objects is the limit; \
             raise it with `--comptime-mem MIB`",
            limits.heap_bytes >> 20
        ));
    }
    if isolate.panicked() {
        return Err(String::new());
    }
    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Native code calls a direct function without the table's stub in the
    /// way, unless the table lists none (`coil_native_host`): a refused or
    /// gated native must not list one.
    #[test]
    fn a_native_the_sandbox_refuses_has_no_direct_function() {
        let natives = sandbox_natives();
        assert!(natives.len() > 100, "the whole host table");
        for native in natives {
            if !native_allowed_at_compile_time(native.name()) {
                assert!(native.direct().is_none(), "`{}` can be called past the sandbox", native.name());
                assert!(native.guarded_direct().is_none(), "`{}` can be called past the sandbox", native.name());
            }
        }
    }
}
