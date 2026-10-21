//! Compile-time execution of user macros on the coil VM.
//!
//! [`VmMacroHost`] implements [`compiler::macros::MacroHost`]: each expansion
//! entry runs on a [`Machine`] with only pure host natives wired (see
//! [`compiler::macros::native_allowed_at_compile_time`]), a step budget and
//! a heap cap ([`ComptimeLimits`], `--comptime-steps` / `--comptime-mem`),
//! so a macro cannot touch the outside world, hang the build or exhaust its
//! memory.
//!
//! Each thread keeps one VM per expansion program, with the natives and the
//! program loaded once (#1008). After a call that returns, the VM is reset
//! like a reused reactor worker ([`Machine::reset_isolate_heap`]: statics,
//! frames and heap back to their initial state), so every call starts as
//! on a fresh VM. A call that panics, runs out of budget or aborts drops the
//! VM; the next call builds a new one.

mod io_gate;

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

use common::{Byte, Value};
use compiler::ComptimeLimits;
use compiler::macros::{CompiledExpansion, ComptimeIo, ConstEval, ConstEvalError, ConstEvalOutcome, FoldCall, FoldValue, MacroHost, native_allowed_at_compile_time};
use machine::{FfiError, HostClosureFn, Machine, NativeFn, Object, SnapRoot};

/// Runs macros in a sandboxed VM.
#[derive(Default)]
pub struct VmMacroHost;

impl VmMacroHost {
    pub fn shared() -> Arc<dyn MacroHost> {
        Arc::new(Self)
    }
}

/// Make every pipeline created after this call run user macros on the VM.
pub fn install() {
    compiler::macros::install_default_host(VmMacroHost::shared());
}

impl MacroHost for VmMacroHost {
    fn run(
        &self,
        program: &CompiledExpansion,
        calls: &[(u32, String)],
        limits: &ComptimeLimits,
        io: &ComptimeIo,
    ) -> Vec<Result<String, String>> {
        let outer = io_gate::set_active(Some(Arc::new(io.clone())));
        let out = calls
            .iter()
            .map(|(entry, input)| run_entry(program, *entry, input, limits))
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

fn const_vm(program: &CompiledExpansion, captured: &Captured) -> Box<Machine<256>> {
    let mut vm = new_vm(program);
    vm.heap_mut()
        .set_class_word_kinds(machine::class_kind_table(&program.class_word_kinds));
    let _ = vm.with_output(captured.clone());
    vm
}

fn snap_roots(vm: &Machine<256>, consts: &[ConstEval], done: &[usize]) -> Vec<SnapRoot> {
    done.iter()
        .map(|&i| SnapRoot {
            slot: consts[i].slot,
            value: vm.static_value(consts[i].slot).unwrap_or_default(),
            kind: consts[i].kind,
        })
        .collect()
}

/// Run each initializer and keep its value in its slot, then snapshot them
/// all, then run the folds. A silent item or fold that fails leaves the VM
/// in an unknown state, so the values computed so far move to a new VM
/// through a snapshot.
fn eval_in(
    program: &CompiledExpansion,
    consts: &[ConstEval],
    folds: &[FoldCall],
    limits: &ComptimeLimits,
    io: &[Arc<ComptimeIo>],
    captured: &Captured,
) -> Result<ConstEvalOutcome, ConstEvalError> {
    let mut vm = const_vm(program, captured);
    let mut done: Vec<usize> = Vec::new();
    let mut skipped: Vec<usize> = Vec::new();
    let mut silent_left = ComptimeLimits::SILENT_MODULE_STEPS;
    for (index, c) in consts.iter().enumerate() {
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
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            call_limited(&mut vm, c.entry, &[], &item_limits)
        }));
        io_gate::set_active(None);
        if c.silent {
            silent_left = silent_left.saturating_sub(vm.steps_charged());
        }
        match outcome {
            Ok(Ok(value)) => {
                vm.set_static(c.slot, value);
                done.push(index);
            }
            Ok(Err(_)) | Err(_) if c.silent => {
                skipped.push(index);
                vm = rebuild(program, consts, &mut done, &mut skipped, vm, captured)?;
            }
            Ok(Err(reason)) => {
                let _ = vm.restore_output();
                return Err(ConstEvalError::Failed {
                    index,
                    reason: failure_reason(reason, captured),
                });
            }
            Err(_) => {
                return Err(ConstEvalError::Failed {
                    index,
                    reason: failure_reason("the VM aborted".to_string(), captured),
                });
            }
        }
    }
    let snapshot = capture(&vm, consts, &mut done, &mut skipped)?;
    skipped.sort_unstable();
    let mut folded = Vec::with_capacity(folds.len());
    for f in folds {
        if silent_left == 0 {
            folded.push(None);
            continue;
        }
        let item_limits = ComptimeLimits {
            steps: ComptimeLimits::SILENT_CALL_STEPS.min(silent_left),
            ..*limits
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let args: Vec<Value> = f
                .args
                .iter()
                .map(|a| match a {
                    FoldValue::Word(w) => Value::from(*w),
                    FoldValue::Text(s) => {
                        let s = vm.heap_mut().intern(s.clone());
                        Value::from(s.as_ptr() as *mut u8 as u64)
                    }
                })
                .collect();
            let ret = call_limited(&mut vm, f.entry, &args, &item_limits)?;
            if !f.text {
                return Ok(FoldValue::Word(ret.raw() as u64));
            }
            match vm.heap().find_object_by_addr(ret.raw() as u64) {
                Some(Object::String(s)) => Ok(FoldValue::Text(s.as_ref().data.to_string())),
                _ => Err("it did not return a string".to_string()),
            }
        }));
        silent_left = silent_left.saturating_sub(vm.steps_charged());
        match outcome {
            Ok(Ok(value)) => folded.push(Some(value)),
            _ => {
                folded.push(None);
                let _ = vm.restore_output();
                vm = restored_vm(program, &snapshot, captured);
            }
        }
    }
    let _ = vm.restore_output();
    Ok(ConstEvalOutcome {
        snapshot,
        skipped,
        folded,
    })
}

/// A new VM whose statics hold `snapshot`'s values.
fn restored_vm(program: &CompiledExpansion, snapshot: &common::HeapSnapshot, captured: &Captured) -> Box<Machine<256>> {
    let mut vm = const_vm(program, captured);
    for (slot, value) in vm.heap_mut().restore_snapshot(snapshot) {
        vm.set_static(slot, value);
    }
    vm
}

/// Snapshot the `done` items' values; a silent item whose value has no
/// snapshot form moves to `skipped`.
fn capture(
    vm: &Machine<256>,
    consts: &[ConstEval],
    done: &mut Vec<usize>,
    skipped: &mut Vec<usize>,
) -> Result<common::HeapSnapshot, ConstEvalError> {
    loop {
        match vm.heap().capture_snapshot(&snap_roots(vm, consts, done)) {
            Ok(snapshot) => return Ok(snapshot),
            Err(e) if consts[done[e.root]].silent => skipped.push(done.remove(e.root)),
            Err(mut e) => {
                e.root = done[e.root];
                return Err(ConstEvalError::Unsnapshottable(e));
            }
        }
    }
}

/// A new VM holding the values computed so far.
fn rebuild(
    program: &CompiledExpansion,
    consts: &[ConstEval],
    done: &mut Vec<usize>,
    skipped: &mut Vec<usize>,
    old: Box<Machine<256>>,
    captured: &Captured,
) -> Result<Box<Machine<256>>, ConstEvalError> {
    let snapshot = capture(&old, consts, done, skipped);
    let mut old = old;
    let _ = old.restore_output();
    drop(old);
    Ok(restored_vm(program, &snapshot?, captured))
}

/// The failure text: the VM's own reason, else what the call printed.
/// What the VM printed before it stopped. A native the host refused says
/// what to grant, and where in a library it was called does not help.
fn printed_text(captured: &Captured) -> String {
    let printed = String::from_utf8_lossy(&captured.0.lock().expect("capture lock")).trim().to_string();
    let (first, rest) = printed.split_once('\n').unwrap_or((&printed, ""));
    match first.strip_prefix("panic: ") {
        Some(msg) if msg.contains(" while compiling") => {
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
        r if r == "the VM aborted" && !printed.is_empty() => printed,
        r => r,
    }
}

/// A thread's VM for one expansion program. Holding the bytecode `Arc`
/// keeps the program alive, so its address never names another one.
struct WarmVm {
    program: Arc<Vec<Byte>>,
    vm: Box<Machine<256>>,
}

thread_local! {
    static WARM: RefCell<Vec<WarmVm>> = const { RefCell::new(Vec::new()) };
}

/// This thread's VM for `program`, or a new one with it loaded.
fn take_vm(program: &CompiledExpansion) -> Box<Machine<256>> {
    let warm = WARM.with(|warm| {
        let mut warm = warm.borrow_mut();
        let at = warm.iter().position(|w| Arc::ptr_eq(&w.program, &program.bytecode))?;
        Some(warm.swap_remove(at).vm)
    });
    warm.unwrap_or_else(|| new_vm(program))
}

fn keep_vm(program: &CompiledExpansion, vm: Box<Machine<256>>) {
    WARM.with(|warm| {
        warm.borrow_mut().push(WarmVm { program: program.bytecode.clone(), vm });
    });
}

fn new_vm(program: &CompiledExpansion) -> Box<Machine<256>> {
    let mut vm = Box::new(Machine::<256>::with_operand_capacity(program.operand_stack_slots as usize));
    for native in sandbox_natives() {
        vm.register_native(native.clone());
    }
    vm.set_program_debug(program.program_debug.clone());
    vm.init_static_slots(program.static_slot_count);
    vm.load_shared_program(
        program.bytecode.clone(),
        program.constants.clone(),
        program.strings.clone(),
    );
    vm
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
    let mut vm = take_vm(program);
    let captured = Captured::default();
    // `with_output` installs a thread-local redirect pointing into the VM;
    // put the caller's back before the VM is dropped.
    let outer = machine::io::set_output_redirect(None);
    let kinds = machine::swap_static_word_kinds(program.static_word_kinds.clone());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = vm.with_output(captured.clone());
        let result = call_entry(&mut vm, entry, input, limits);
        let _ = vm.restore_output();
        result
    }));
    machine::swap_static_word_kinds(kinds);
    machine::io::set_output_redirect(outer);
    // Only a call that returned leaves the VM between frames; any other
    // outcome drops it.
    if matches!(outcome, Ok(Ok(_))) {
        vm.reset_isolate_heap();
        keep_vm(program, vm);
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
            "the VM aborted".to_string()
        } else {
            printed
        }),
    }
}

fn call_entry(vm: &mut Machine<256>, entry: u32, input: &str, limits: &ComptimeLimits) -> Result<String, String> {
    let input = vm.heap_mut().intern(input.to_string());
    let arg = Value::from(input.as_ptr() as *mut u8 as u64);
    let ret = call_limited(vm, entry, &[arg], limits)?;
    match vm.heap().find_object_by_addr(ret.raw() as u64) {
        Some(Object::String(s)) => Ok(s.as_ref().data.to_string()),
        _ => Err("it did not return a string".to_string()),
    }
}

/// Call `entry` within `limits`. An empty error is a plain panic (its
/// message went to the VM's output).
fn call_limited(vm: &mut Machine<256>, entry: u32, args: &[Value], limits: &ComptimeLimits) -> Result<Value, String> {
    vm.set_step_budget(Some(limits.steps));
    vm.set_heap_cap(Some(usize::try_from(limits.heap_bytes).unwrap_or(usize::MAX)));
    let ret: Value = vm.call_function(entry, args);
    if vm.step_budget_exhausted() {
        return Err(format!(
            "it ran too long: {} steps (loop iterations + calls) is the limit; \
             is there an infinite loop? Raise it with `--comptime-steps N`",
            limits.steps
        ));
    }
    if vm.heap_cap_exceeded() {
        return Err(format!(
            "it used too much memory: {} MiB of live objects is the limit; \
             raise it with `--comptime-mem MIB`",
            limits.heap_bytes >> 20
        ));
    }
    if vm.panicked() {
        return Err(String::new());
    }
    Ok(ret)
}
