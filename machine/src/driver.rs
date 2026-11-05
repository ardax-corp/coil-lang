//! A program run by its native code alone, without the interpreter
//! (`docs/internals/native.md`, "The native driver"). The driver owns the
//! [`Runtime`], walks the prologue's startup (`RestoreSnapshot`, the
//! static initializers and `__module_setup`, which are native calls) and
//! calls `main` or a test case natively. Native code reaches the driver
//! through its [`NativeHost`]: calls of other bodies and of function
//! values run natively, host natives on the Runtime, collections over the
//! native frames' root records ([`GcHost`]).
//!
//! Child tasks run on fibers of their own. The root task (the program's
//! own call) never leaves its stack: where it waits, the driver runs the
//! scheduler there until the root is ready again; a child that waits
//! suspends its fiber back to that loop.


use std::ffi::c_void;
use std::sync::Arc;

use common::{Instruction, ProgramDebug, Value};

use crate::collect::{GcHost, HostRoot};
use crate::native::{Invoked, NativeCode, NativeCtx, NativeEnv, NativeHost, NativeOutcome, NativePanic};
use crate::runtime::{HostEnv, Runtime};

/// `gen_close`, the generator close native code calls when a loop leaves
/// a generator early: its offset from `TASK_SCOPE_OPEN_ID`.
const GEN_CLOSE_OP: usize = 45;

#[cfg(not(target_family = "wasm"))]
thread_local! {
    /// The running child task's yielder (null while the root runs). Per
    /// thread, as [`crate::native::GEN_YIELDER`]: the fiber body sets it, out
    /// of sight of the code that resumed the fiber.
    static TASK_YIELDER: std::cell::Cell<*const ()> = const { std::cell::Cell::new(std::ptr::null()) };
}

/// What a task fiber is resumed with: its wait's wake, or a cancel.
#[cfg(not(target_family = "wasm"))]
enum TaskResume {
    Wake(Value),
    Cancel,
}

#[cfg(not(target_family = "wasm"))]
type TaskCoro = corosensei::Coroutine<TaskResume, (), (), corosensei::stack::DefaultStack>;

/// A child task's fiber.
#[cfg(not(target_family = "wasm"))]
struct TaskFiber {
    /// Taken when the body returned (its stack is reused).
    coro: Option<TaskCoro>,
    /// The lowest address native code on it may use.
    stack_limit: usize,
    /// Its yielder, once it started.
    yielder: *const (),
    /// The roots of the side not running: the task's native calls,
    /// argument roots and call depth while it is parked, the root task's
    /// while it runs ([`Driver::swap_roots`]).
    native_roots: Vec<(*mut NativeCtx, usize)>,
    arg_roots: Vec<Value>,
    depth: usize,
    gen_running: Vec<Value>,
}

#[cfg(not(target_family = "wasm"))]
impl Drop for TaskFiber {
    fn drop(&mut self) {
        if let Some(coro) = &mut self.coro
            && coro.started()
            && !coro.done()
        {
            // A task that never resumes (the root task panicked): its
            // native frames cannot unwind, so the stack is reclaimed as
            // it is; what they own leaks.
            // SAFETY: nothing runs on the fiber's stack again.
            unsafe { coro.force_reset() };
        }
    }
}

/// Whether programs native code covers run on the driver
/// (`COIL_NATIVE_DRIVER=0`: the VM runs them, calling native code per body).
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("COIL_NATIVE_DRIVER").is_none_or(|v| v != "0"))
}

/// The program a driver runs: its code (for the prologue and the
/// snapshot pool), the native code for it and its debug tables.
pub(crate) struct DriverProgram {
    pub(crate) code: Arc<Vec<common::Byte>>,
    pub(crate) constants: Arc<Vec<u64>>,
    pub(crate) native: Arc<NativeCode>,
    pub(crate) debug: ProgramDebug,
    pub(crate) base_dir: Option<std::path::PathBuf>,
    /// What a thread it spawns runs (`None`: it spawns none).
    pub(crate) thread_program: Option<Arc<crate::thread::ThreadProgram>>,
    /// Where spawned threads print (the test harness's capture).
    pub(crate) shared_print: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
}

/// Runs a program's native code with no interpreter.
pub(crate) struct Driver<'a> {
    pub(crate) rt: &'a mut Runtime,
    program: DriverProgram,
    /// A panic was reported: the run stops.
    panicked: bool,
    /// A `drop` the collector ran panicked: no later finalizer runs.
    finalizer_aborted: bool,
    /// Native calls in progress (what a [`Runtime::native_roots`] entry
    /// records).
    depth: usize,
    /// Stacks of finished generator fibers, for the next ones.
    #[cfg(not(target_family = "wasm"))]
    gen_stacks: Vec<corosensei::stack::DefaultStack>,
    /// Generators whose body runs now, innermost last (roots meanwhile).
    gen_running: Vec<Value>,
    /// Child tasks' fibers, by task id.
    #[cfg(not(target_family = "wasm"))]
    tasks: std::collections::HashMap<crate::task::TaskId, Box<TaskFiber>>,
    /// Stacks of finished task fibers, for the next ones.
    #[cfg(not(target_family = "wasm"))]
    task_stacks: Vec<corosensei::stack::DefaultStack>,
    /// A child task's panic message: its scope reports it.
    task_panic: Option<String>,
    /// The next panic reported prints no location (a contract failure
    /// names its call site in the message).
    panic_without_location: bool,
    /// The last panic's message.
    panic_message: String,
}

impl<'a> Driver<'a> {
    const HOST: NativeHost = NativeHost {
        print: Driver::host_print,
        call: Driver::host_call,
        call_value: Driver::host_call_value,
        claim: Driver::host_claim,
        resume: Driver::host_resume,
        collect: Driver::host_collect,
        gc: Driver::host_gc,
        free: Driver::host_free,
        host: |_, _, _, _, _, _| None,
        ffi: Driver::host_ffi,
        invoke: Driver::host_invoke,
    };

    pub(crate) fn new(rt: &'a mut Runtime, program: DriverProgram) -> Self {
        Self {
            rt,
            program,
            panicked: false,
            finalizer_aborted: false,
            depth: 0,
            #[cfg(not(target_family = "wasm"))]
            gen_stacks: Vec::new(),
            gen_running: Vec::new(),
            #[cfg(not(target_family = "wasm"))]
            tasks: std::collections::HashMap::new(),
            #[cfg(not(target_family = "wasm"))]
            task_stacks: Vec::new(),
            task_panic: None,
            panic_without_location: false,
            panic_message: String::new(),
        }
    }

    /// The last panic's message (a thread job's join reports it).
    pub(crate) fn take_panic_message(&mut self) -> String {
        std::mem::take(&mut self.panic_message)
    }

    pub(crate) fn panicked(&self) -> bool {
        self.panicked
    }

    /// What a thread the program spawns runs with.
    pub(crate) fn thread_spawn_context(&self) -> Option<crate::thread::ThreadSpawnContext> {
        self.rt.thread_spawn_context(self.program.thread_program.clone()?, self.program.shared_print.clone())
    }

    /// Whether the driver can run the program that starts at `ip`: its
    /// prologue holds only startup ops, and every body it calls compiled.
    pub(crate) fn can_start(code: &[common::Byte], native: &NativeCode, ip: usize) -> bool {
        let mut pc = ip;
        for _ in 0..code.len() {
            let Some(b) = code.get(pc) else { return false };
            match *b.bytecode() {
                Instruction::CALL => {
                    // `CALL 0`: the prologue's bootstrap frame, no call.
                    let t = b.call_parts().1;
                    if t != 0 && native.lookup(t).is_none() {
                        return false;
                    }
                }
                Instruction::StoreStatic | Instruction::POP | Instruction::NOOP | Instruction::RestoreSnapshot => {}
                // The prologue's end, or the `CALL main; RETURN` its jump
                // into `main` leads to with native code.
                Instruction::HALT | Instruction::RETURN => return true,
                Instruction::JMP => {
                    let t = b.operand_u32() as usize;
                    if native.lookup(t).is_some() {
                        return true;
                    }
                    pc = t;
                    continue;
                }
                _ => return false,
            }
            pc += 1;
        }
        false
    }

    /// Run the prologue from `ip` (as [`Self::can_start`] checked it):
    /// the startup ops, until its `HALT` or its jump into `main`, which
    /// is called. Stops at a panic.
    pub(crate) fn run_from(&mut self, ip: usize) {
        let code = Arc::clone(&self.program.code);
        let mut pc = ip;
        let mut last = Value::default();
        while !self.panicked {
            let Some(b) = code.get(pc) else { return };
            match *b.bytecode() {
                Instruction::CALL if b.call_parts().1 == 0 => {}
                Instruction::CALL => last = self.call(b.call_parts().1, &[]).map_or_else(Value::default, |(lo, _)| Value::from(lo)),
                Instruction::StoreStatic => {
                    if let Some(s) = self.rt.statics.get_mut(b.operand_u32() as usize) {
                        *s = last;
                    }
                }
                Instruction::POP | Instruction::NOOP => {}
                Instruction::RestoreSnapshot => self.restore_snapshot(b.operand_u32() as usize),
                Instruction::JMP => {
                    let t = b.operand_u32() as usize;
                    if self.program.native.lookup(t).is_some() {
                        let _ = self.call(t, &[]);
                        return;
                    }
                    pc = t;
                    continue;
                }
                _ => return,
            }
            pc += 1;
        }
    }

    /// `RestoreSnapshot` of the snapshot at pool index `at`.
    fn restore_snapshot(&mut self, at: usize) {
        let snapshot = self
            .program
            .constants
            .get(at..)
            .ok_or(common::SnapshotDecodeError::Truncated)
            .and_then(common::HeapSnapshot::decode);
        let snapshot = match snapshot {
            Ok(s) if s.statics.iter().all(|(slot, _)| (*slot as usize) < self.rt.statics.len()) => s,
            Ok(_) => return self.report("RestoreSnapshot: static slot out of range", None, Vec::new(), false),
            Err(e) => return self.report(&format!("RestoreSnapshot: {e}"), None, Vec::new(), false),
        };
        for (slot, value) in self.rt.heap.restore_snapshot(&snapshot) {
            self.rt.statics[slot as usize] = value;
        }
    }

    /// The driver as native code reaches it.
    fn env(&mut self) -> NativeEnv {
        let strings_len = self.rt.program_strings.len().min(self.rt.program_string_cache.len());
        NativeEnv {
            heap: std::ptr::from_mut::<crate::memory::Heap>(&mut self.rt.heap),
            strings: self.rt.program_strings.as_ptr(),
            string_cache: self.rt.program_string_cache.as_mut_ptr(),
            strings_len,
            has_finalizers: !self.rt.finalizer_by_type.is_empty(),
            statics: self.rt.statics.as_mut_ptr(),
            statics_len: self.rt.statics.len(),
            machine: std::ptr::from_mut(self).cast(),
            host: Self::HOST,
            fuel: &raw mut self.rt.fuel,
            natives: &raw const self.rt.natives,
        }
    }

    /// Call the body at `pc` on `args` natively: its result words, or
    /// `None` when it panicked (reported, its pads run).
    pub(crate) fn call(&mut self, pc: usize, args: &[Value]) -> Option<(u64, Option<u64>)> {
        let Some(entry) = self.program.native.lookup(pc) else {
            let name = self.locator().fn_symbol_at(pc).unwrap_or("<unknown>").to_string();
            self.report(&format!("native driver: `{name}` has no native code"), None, Vec::new(), false);
            return None;
        };
        let params = entry.arity as usize;
        if params > 16 || args.len() < entry.reads as usize {
            self.report("native driver: a call with more than 16 words", None, Vec::new(), false);
            return None;
        }
        let mut words = [0u64; 16];
        for (w, a) in words.iter_mut().zip(args.iter().take(params)) {
            *w = a.bits();
        }
        if let Some(t) = self.rt.native.as_mut() {
            t.hits += 1;
        }
        let env = self.env();
        self.depth += 1;
        #[cfg(not(target_family = "wasm"))]
        let out = crate::stacks::on_deep_stack(|| crate::native::call(&entry, &words[..params], &env));
        #[cfg(target_family = "wasm")]
        let out = crate::native::call(&entry, &words[..params], &env);
        self.depth -= 1;
        match out {
            NativeOutcome::Done(r) => {
                for (w, deep) in r.frees {
                    if !self.free_early(Value::from(w), deep) {
                        return None;
                    }
                }
                if r.allocated {
                    // The result is the caller's word: rooted meanwhile.
                    let n = self.rt.ffi_arg_roots.len();
                    self.rt.ffi_arg_roots.push(Value::from(r.lo));
                    self.rt.ffi_arg_roots.extend(r.hi.map(Value::from));
                    crate::collect::safepoint(self);
                    self.rt.ffi_arg_roots.truncate(n);
                    if self.panicked {
                        return None;
                    }
                }
                Some((r.lo, r.hi))
            }
            NativeOutcome::Panicked(p) => {
                self.native_panic(p);
                None
            }
            NativeOutcome::Bailed => {
                self.report("native driver: a native call could not start (no stack)", None, Vec::new(), false);
                None
            }
        }
    }

    /// Call function word `callee` on `args`, the last `dicts` of them
    /// instance dictionaries, as `CallIndirect` does.
    pub(crate) fn call_value(&mut self, callee: Value, args: &[Value], dicts: usize) -> Option<Value> {
        let (mut callee, mut args, mut dicts) = (callee, args.to_vec(), dicts);
        loop {
            let code_len = self.program.code.len();
            match crate::apply::apply(&mut self.rt.heap, callee, &args, dicts, code_len) {
                crate::apply::Apply::Value(v) => return Some(v),
                crate::apply::Apply::Bad(msg) => {
                    self.report(&msg, None, Vec::new(), false);
                    return None;
                }
                crate::apply::Apply::Call { pc, args: call, extras } => {
                    // The extras wait rooted for the result.
                    let n = self.rt.ffi_arg_roots.len();
                    self.rt.ffi_arg_roots.extend_from_slice(&extras);
                    let out = self.call(pc, &call);
                    self.rt.ffi_arg_roots.truncate(n);
                    let (lo, _) = out?;
                    if extras.is_empty() {
                        return Some(Value::from(lo));
                    }
                    (callee, args, dicts) = (Value::from(lo), extras, 0);
                }
            }
        }
    }

    /// Resume generator `handle` with `send`: the word it yields or
    /// returns, `None` when it panicked.
    fn resume(&mut self, handle: Value, send: Value) -> Option<Value> {
        let Some(crate::memory::Object::Coroutine(gc)) = self.rt.heap.find_object_by_addr(handle.bits()) else {
            self.report("resumed invalid coroutine handle", None, Vec::new(), false);
            return None;
        };
        match crate::gen_fiber::step(self, gc, send) {
            Some(crate::gen_fiber::GenStep::Yielded(v) | crate::gen_fiber::GenStep::Returned(v)) => Some(v),
            Some(crate::gen_fiber::GenStep::Panicked(p)) => {
                self.native_panic(p);
                None
            }
            None => {
                // A body with no `yield` compiles as a plain function: the
                // first resume runs it to its return.
                let c = gc.as_ref();
                let plain = !c.started && c.state == crate::memory::CoroState::Suspended && c.yield_from.is_none();
                if plain && self.program.native.lookup(c.resume_ip).is_some_and(|e| !e.generator) {
                    let (pc, args) = (c.resume_ip, c.saved_stack.clone());
                    gc.payload_mut().started = true;
                    let out = self.call(pc, &args);
                    crate::gen_fiber::finish(gc);
                    return out.map(|(lo, _)| Value::from(lo));
                }
                self.report("native driver: a generator without native code", None, Vec::new(), false);
                None
            }
        }
    }

    /// `coro_close` of `handle` (a loop left a generator early): its body
    /// unwinds from its yield, running its armed `defer`s. `false` when one
    /// panicked (reported).
    fn close_now(&mut self, handle: Value) -> bool {
        let Some(crate::memory::Object::Coroutine(gc)) = self.rt.heap.find_object_by_addr(handle.bits()) else {
            return true;
        };
        // A delegating generator is suspended in its innermost delegate.
        let mut target = gc;
        while let Some(sub) = target.as_ref().yield_from {
            target = sub;
        }
        #[cfg(not(target_family = "wasm"))]
        let pads = crate::gen_fiber::close(self, target);
        #[cfg(target_family = "wasm")]
        let pads: Option<Vec<crate::native::NativePad>> = None;
        let mut link = Some(gc);
        while let Some(c) = link {
            link = c.as_ref().yield_from;
            crate::gen_fiber::finish(c);
        }
        if let Some(pads) = pads {
            self.run_pads(pads);
        }
        !self.panicked
    }

    /// Close generator `val` nothing reaches any more: its armed `defer`s
    /// run; a panic in one ends the program, as one in a `drop` does.
    fn close_generator(&mut self, val: Value) {
        if self.finalizer_aborted {
            return;
        }
        let Some(crate::memory::Object::Coroutine(gc)) = self.rt.heap.find_object_by_addr(val.bits()) else { return };
        #[cfg(not(target_family = "wasm"))]
        if let Some(pads) = crate::gen_fiber::close(self, gc) {
            let was = std::mem::replace(&mut self.panicked, false);
            self.run_pads(pads);
            if self.panicked {
                self.abort_after_finalizer_panic();
                return;
            }
            self.panicked = was;
        }
        #[cfg(target_family = "wasm")]
        crate::gen_fiber::finish(gc);
    }

    /// A native panic, unless it is one a nested call already reported
    /// ([`crate::native::TRAP_VM_PANIC`]); then its frames' pads run.
    fn native_panic(&mut self, p: NativePanic) {
        if p.code != crate::native::TRAP_VM_PANIC {
            let code = Arc::clone(&self.program.native);
            let locator = self.locator();
            // A stack overflow traps in the prologue of a frame that never
            // started: the panic is at the call that made it.
            let skip = usize::from(p.code == crate::native::TRAP_STACK);
            let mut loc = None;
            let mut frames = Vec::new();
            for (i, &s) in p.trace.iter().skip(skip).enumerate() {
                let Some(site) = code.site(s) else { continue };
                if i == 0 {
                    loc = locator.native_site_location(site);
                }
                frames.extend(locator.native_site_line(&code, s));
            }
            // The native calls under this one, innermost first.
            for &(ctx, _) in self.rt.native_roots.iter().rev() {
                // SAFETY: a native call in progress keeps its context alive.
                for s in unsafe { &*ctx }.call_sites() {
                    frames.extend(locator.native_site_line(&code, s));
                }
            }
            let explicit = p.code == crate::native::TRAP_EXPLICIT;
            let message = if p.code == crate::native::TRAP_BUDGET { self.budget_panic() } else { Some(p.message()) };
            if let Some(message) = message {
                self.report(message, loc, frames, explicit);
            }
        }
        self.panicked = true;
        self.run_pads(p.pads);
    }

    /// Fuel reached zero: the panic to report, `None` when it was reported
    /// already. Leaves one unit, so any later charge stops again.
    fn budget_panic(&mut self) -> Option<&'static str> {
        self.rt.fuel = 1;
        if self.rt.heap_cap_hit {
            return (!self.panicked).then_some(crate::runtime::HEAP_CAP_EXCEEDED);
        }
        if std::mem::replace(&mut self.rt.step_budget_hit, true) {
            return None;
        }
        Some(crate::runtime::STEP_BUDGET_EXHAUSTED)
    }

    /// A panic the driver raises for the host call native code makes
    /// through `ctx`: at that call, with its native callers' backtrace.
    fn report_at(&mut self, ctx: *mut NativeCtx, message: &str) {
        let code = Arc::clone(&self.program.native);
        let locator = self.locator();
        // SAFETY: the calling native code's live context.
        let sites = unsafe { &*ctx }.call_sites();
        let loc = sites.first().and_then(|&s| locator.native_site_location(code.site(s)?));
        let mut frames: Vec<String> = sites.iter().filter_map(|&s| locator.native_site_line(&code, s)).collect();
        for &(c, _) in self.rt.native_roots.iter().rev() {
            // SAFETY: a native call in progress keeps its context alive.
            for s in unsafe { &*c }.call_sites() {
                frames.extend(locator.native_site_line(&code, s));
            }
        }
        self.report(message, loc, frames, false);
    }

    /// Run the cleanup pads a panic unwound; a panic in one is reported and
    /// the rest still run.
    fn run_pads(&mut self, pads: Vec<crate::native::NativePad>) {
        if pads.is_empty() {
            return;
        }
        let env = self.env();
        let mut raised = Vec::new();
        crate::native::run_pads(pads, &env, &mut |p| raised.push(p));
        for p in raised {
            self.native_panic(p);
        }
    }

    /// Print a panic as the VM does: `panic: message at loc`, then the
    /// backtrace (an explicit `panic` only under `COIL_BACKTRACE`).
    fn report(&mut self, message: &str, loc: Option<String>, frames: Vec<String>, explicit: bool) {
        use std::io::Write;
        self.panicked = true;
        self.panic_message = message.to_string();
        let without_location = std::mem::take(&mut self.panic_without_location);
        if self.rt.sched.as_ref().is_some_and(|s| s.current != crate::task::ROOT) {
            // A child task's panic fails its scope, which reports it.
            self.task_panic = Some(message.to_string());
            return;
        }
        let loc = loc.filter(|_| !without_location);
        let loc_suffix = loc.map(|l| format!(" at {l}")).unwrap_or_default();
        let show = !explicit || std::env::var("COIL_BACKTRACE").is_ok_and(|v| !v.is_empty() && v != "0");
        let mut lines: Vec<String> = Vec::new();
        let mut repeats = 0usize;
        for line in frames.into_iter().filter(|_| show) {
            if lines.last() == Some(&line) {
                repeats += 1;
                continue;
            }
            if repeats > 0 {
                lines.push(format!("  ... repeated {repeats} more times"));
                repeats = 0;
            }
            lines.push(line);
        }
        if repeats > 0 {
            lines.push(format!("  ... repeated {repeats} more times"));
        }
        let mut text = format!("panic: {message}{loc_suffix}");
        if !lines.is_empty() {
            text.push('\n');
            text.push_str(&lines.join("\n"));
        }
        if let Some(out) = self.rt.output.as_mut() {
            let _ = write!(out, "{text}");
            let _ = out.flush();
            self.rt.mirror_stderr(&text);
        } else {
            eprintln!("{text}");
            let _ = std::io::stderr().flush();
        }
    }

    fn locator(&self) -> crate::locate::Locator<'_> {
        crate::locate::Locator { debug: &self.program.debug, code_len: self.program.code.len(), base_dir: self.program.base_dir.as_deref() }
    }

    /// `FreeEarly` of `v`: the `drop`s it owes run first. `false` when one
    /// panicked.
    fn free_early(&mut self, v: Value, deep: bool) -> bool {
        let mode = crate::memory::early_free_mode();
        if !self.rt.finalizer_by_type.is_empty() && mode == crate::memory::EarlyFreeMode::Free {
            let Some(drops) = self.rt.heap.early_drops(v, deep, &|_| false) else {
                return true;
            };
            if !drops.is_empty() {
                // Rooted while they run: nothing else reaches them.
                let n = self.rt.ffi_arg_roots.len();
                self.rt.ffi_arg_roots.push(v);
                self.rt.ffi_arg_roots.extend(drops.iter().map(|o| Value::from(o.addr())));
                for obj in &drops {
                    let Some(pc) = self.rt.pending_finalizer(*obj) else { continue };
                    let val = Value::from(obj.addr());
                    if self.finalizer_aborted || !self.rt.claim_finalizer(val) {
                        continue;
                    }
                    if self.call(pc as usize, &[val]).is_none() {
                        self.rt.ffi_arg_roots.truncate(n);
                        return false;
                    }
                }
                self.rt.ffi_arg_roots.truncate(n);
            }
        }
        if mode != crate::memory::EarlyFreeMode::Off {
            self.rt.heap.free_early(v, deep, mode == crate::memory::EarlyFreeMode::Check, &|_| false);
        }
        true
    }

    /// Run the `drop`s still owed, as a program's end does.
    pub(crate) fn run_remaining_finalizers(&mut self) {
        if self.rt.finalizer_by_type.is_empty() && !self.rt.closeables {
            return;
        }
        let queue: Vec<_> =
            self.rt.heap.objects().filter_map(|obj| Some((Value::from(obj.addr()), self.rt.pending_cleanup(obj)?))).collect();
        for (val, pc) in queue {
            GcHost::run_pending(self, val, pc);
        }
    }

    /// A `drop` the collector or the program's end ran panicked: the
    /// program ends (under the test harness, only the job stops).
    fn abort_after_finalizer_panic(&mut self) {
        if !crate::env::exit_trapped() {
            use std::io::Write;
            if let Some(out) = self.rt.output.as_mut() {
                let _ = out.flush();
            }
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(1);
        }
        self.finalizer_aborted = true;
    }

    /// Run `f` with `ctx`'s words rooted.
    fn rooted<R>(&mut self, ctx: *mut NativeCtx, f: impl FnOnce(&mut Self) -> R) -> R {
        self.rt.native_roots.push((ctx, self.depth));
        let out = f(self);
        self.rt.native_roots.pop();
        out
    }

    /// The driver native code passes back.
    ///
    /// # Safety
    /// `raw` is the driver [`Self::env`] gave the call.
    unsafe fn from_raw<'b>(raw: *mut c_void) -> &'b mut Self {
        // SAFETY: as the caller promises.
        unsafe { &mut *raw.cast::<Self>() }
    }

    unsafe fn host_print(raw: *mut c_void, s: &str) {
        // SAFETY: native code passes back the driver it was given.
        unsafe { Self::from_raw(raw) }.rt.print(s);
    }

    unsafe fn host_call(raw: *mut c_void, ctx: *mut NativeCtx, pc: u32, args: &[Value], panicked: &mut bool) -> u64 {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let out = d.rooted(ctx, |d| d.call(pc as usize, args));
        *panicked = out.is_none();
        let (lo, hi) = out.unwrap_or_default();
        // SAFETY: the calling native code's live context.
        unsafe { (*ctx).vm_hi = hi.unwrap_or(0) };
        lo
    }

    unsafe fn host_call_value(raw: *mut c_void, ctx: *mut NativeCtx, callee: u64, args: &[Value], dicts: u32, panicked: &mut bool) -> u64 {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let out = d.rooted(ctx, |d| d.call_value(Value::from(callee), args, dicts as usize));
        *panicked = out.is_none();
        out.unwrap_or_default().bits()
    }

    unsafe fn host_claim(raw: *mut c_void, pc: u32, word: u64) -> u64 {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        if !d.rt.finalizer_pcs.contains(&pc) {
            return 1;
        }
        if d.rt.claim_finalizer(Value::from(word)) { 2 } else { 0 }
    }

    unsafe fn host_resume(raw: *mut c_void, ctx: *mut NativeCtx, handle: u64, send: Option<u64>, panicked: &mut bool) -> u64 {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        if let Some(crate::memory::Object::Coroutine(gc)) = d.rt.heap.find_object_by_addr(handle)
            && gc.as_ref().state == crate::memory::CoroState::Done
        {
            d.report_at(ctx, "resumed after completion");
            *panicked = true;
            return 0;
        }
        let out = d.rooted(ctx, |d| d.resume(Value::from(handle), send.map_or_else(|| Value::from(0_i64), Value::from)));
        *panicked = out.is_none();
        out.unwrap_or_default().bits()
    }

    unsafe fn host_collect(raw: *mut c_void, ctx: *mut NativeCtx) {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        d.rooted(ctx, crate::collect::safepoint);
    }

    unsafe fn host_gc(raw: *mut c_void, ctx: *mut NativeCtx, panicked: &mut bool) -> u64 {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let before = d.rt.heap.size();
        d.rooted(ctx, crate::collect::collect);
        *panicked = d.panicked;
        before.saturating_sub(d.rt.heap.size()) as u64
    }

    unsafe fn host_free(raw: *mut c_void, ctx: *mut NativeCtx, word: u64, deep: bool) -> bool {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        d.rooted(ctx, |d| d.free_early(Value::from(word), deep)) && !d.panicked
    }

    unsafe fn host_ffi(raw: *mut c_void, ctx: *mut NativeCtx, op: u64, operand: u64, args: &[Value], panicked: &mut bool) -> Option<u64> {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let arg = |i: usize| args.get(i).copied().unwrap_or_default();
        let out = match op as u8 {
            o if o == Instruction::FfiLoad as u8 => d.rt.ffi_load(arg(0)),
            o if o == Instruction::DeclareFFI as u8 => d.rt.ffi_declare(operand as u32, arg(0), arg(1), arg(2), arg(3)),
            _ => {
                let call = crate::runtime_ffi::FfiCall::from_words(&d.rt.heap, args);
                d.rooted(ctx, |d| {
                    // A callback the C function makes runs that body natively.
                    let me = std::ptr::from_mut(d).cast::<c_void>();
                    let layouts = Arc::clone(&d.rt.ffi.struct_layouts);
                    let mut callback = |offset: u32| {
                        let cif = crate::ffi::callback_cif(&[crate::memory::FfiType::Int], crate::memory::FfiType::Int, &layouts)?;
                        crate::ffi::make_int_callback(me, offset, Driver::ffi_callback, cif)
                    };
                    d.rt.ffi_invoke(call.lib_addr, call.function_id, &call.args, call.arg_types.as_deref(), &mut callback)
                })
            }
        };
        *panicked = d.panicked;
        Some(out.bits())
    }

    /// An FFI callback into the body at `offset` on `len` words at `args`.
    unsafe fn ffi_callback(raw: *mut c_void, offset: u32, args: *const Value, len: usize) -> Value {
        // SAFETY: the driver made the callback and runs the C call.
        let d = unsafe { Self::from_raw(raw) };
        // SAFETY: libffi passes the callback's argument words.
        let args = unsafe { std::slice::from_raw_parts(args, len) };
        d.call(offset as usize, args).map_or_else(Value::default, |(lo, _)| Value::from(lo))
    }

    unsafe fn host_invoke(raw: *mut c_void, ctx: *mut NativeCtx, id: u64, layout: u64, args: &[Value]) -> Option<Invoked> {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let fn_id = id as usize;
        let out = match d.rt.natives.get_by_id(fn_id)?.host_op() {
            crate::HostOp::Ordinary => crate::runtime::invoke_ordinary(d, ctx, fn_id, layout, args),
            crate::HostOp::RegisterFinalizer => {
                let word = |i: usize| args.get(i).map_or(0, |v| v.as_int() as u32);
                d.rt.register_finalizer(word(0), word(1));
                Some(Invoked::Value(0))
            }
            op @ (crate::HostOp::RegionEnter | crate::HostOp::RegionExit) => {
                d.rooted(ctx, |d| crate::collect::region(d, op == crate::HostOp::RegionEnter));
                Some(if d.panicked { Invoked::Unwound } else { Invoked::Value(0) })
            }
            crate::HostOp::Task if fn_id == common::TASK_SCOPE_OPEN_ID as usize + GEN_CLOSE_OP => {
                let handle = args.first().copied().unwrap_or_default();
                Some(if d.rooted(ctx, |d| d.close_now(handle)) { Invoked::Value(0) } else { Invoked::Unwound })
            }
            crate::HostOp::Task => Some(d.task_op(ctx, fn_id, args)),
            _ => Some(Invoked::Panic("native driver: unknown host op".into())),
        };
        if out.is_some() {
            crate::native::note_host_call(fn_id, crate::native::HostPath::Runtime);
        }
        out
    }
}

/// Tasks: the scheduler natives, waits and the scheduler loop.
impl Driver<'_> {
    /// A scheduler with live tasks: a wait switches tasks (anywhere, a
    /// generator's body too: its fiber stays where it is, under the task's).
    fn tasks_can_switch(&self) -> bool {
        self.rt.sched.as_ref().is_some_and(|s| s.live > 0)
    }

    /// Scheduler native `fn_id` (`HostOp::Task`) on `args`.
    fn task_op(&mut self, ctx: *mut NativeCtx, fn_id: usize, args: &[Value]) -> Invoked {
        use crate::task::{Block, STATUS_OK, Scheduler};
        let arg = |i: usize| args.get(i).map_or(0, |v| v.as_int());
        let unit = Invoked::Value(0);
        match fn_id.saturating_sub(common::TASK_SCOPE_OPEN_ID as usize) {
            0 => self.task_scope_open(),
            1 => self.task_scope_close(ctx, arg(0)),
            2 => {
                let msg = self.rt.sched.as_mut().and_then(|s| s.remove_scope(arg(0))).and_then(|sc| sc.failed).unwrap_or_default();
                self.rt.task_drop_idle_scheduler();
                Invoked::Value(self.rt.task_string(msg).bits())
            }
            3 => {
                let coro = match self.rt.heap.find_object_by_addr(args.get(1).copied().unwrap_or_default().bits()) {
                    Some(crate::memory::Object::Coroutine(gc)) => gc,
                    _ => return Invoked::Panic("task spawn: not a coroutine".into()),
                };
                let Some(s) = self.rt.sched.as_mut() else {
                    return Invoked::Panic("spawn outside a task scope".into());
                };
                if !s.scopes.get(&arg(0)).is_some_and(|sc| !sc.closed) {
                    return Invoked::Panic("spawn on a task scope that has ended".into());
                }
                Invoked::Value(Value::from(s.add_task(coro, arg(0)) as i64).bits())
            }
            4 => {
                let id = arg(0) as u64;
                let Some(s) = self.rt.sched.as_mut() else {
                    // Finished and forgotten with its scope.
                    return Invoked::Value(Value::from(STATUS_OK).bits());
                };
                let current = s.current;
                let Some(rec) = s.tasks.get_mut(&id) else {
                    return Invoked::Value(Value::from(STATUS_OK).bits());
                };
                if rec.state.finished() {
                    return Invoked::Value(Value::from(Scheduler::status_of(rec.state)).bits());
                }
                if id == current {
                    return Invoked::Panic("a task cannot join itself".into());
                }
                rec.joiners.push(current);
                if !self.tasks_can_switch() {
                    return Invoked::Panic("cannot wait for a task inside a native callback".into());
                }
                self.wait(ctx, Some(Block::Join(id)))
            }
            5 => {
                let msg = self.rt.sched.as_ref().and_then(|s| s.tasks.get(&(arg(0) as u64))).and_then(|r| r.panic.clone()).unwrap_or_default();
                Invoked::Value(self.rt.task_string(msg).bits())
            }
            6 => {
                if !self.tasks_can_switch() {
                    crate::clock::sleep_ms(arg(0));
                    return unit;
                }
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(arg(0).max(0) as u64);
                let s = self.rt.sched.as_mut().expect("tasks_can_switch");
                let current = s.current;
                s.add_timer(current, deadline);
                self.wait(ctx, Some(Block::Sleep))
            }
            7 => {
                if self.tasks_can_switch() && self.rt.sched.as_ref().is_some_and(|s| !s.run_queue.is_empty()) {
                    return self.wait(ctx, None);
                }
                unit
            }
            9 => {
                self.rt.task_request_cancel(arg(0) as u64);
                unit
            }
            10 => {
                if let Some(s) = self.rt.sched.as_mut()
                    && let Some(rec) = s.tasks.get_mut(&s.current)
                {
                    rec.shield += 1;
                }
                unit
            }
            11 => self.task_shield_exit(ctx),
            12 => Invoked::Value(Value::from(crate::task::new_cond()).bits()),
            13 => {
                if !self.tasks_can_switch() {
                    return Invoked::Value(Value::from(crate::task::STATUS_DEADLOCK).bits());
                }
                let s = self.rt.sched.as_mut().expect("tasks_can_switch");
                let current = s.current;
                s.cond_waits.entry(arg(0)).or_default().push(current);
                self.wait(ctx, Some(Block::Cond(arg(0))))
            }
            14 => {
                if let Some(s) = self.rt.sched.as_mut() {
                    s.notify_cond(arg(0));
                }
                unit
            }
            15 => Invoked::Panic(self.contract_failure(ctx, args.first().copied().unwrap_or_default())),
            _ => Invoked::Panic(format!("HostInvoke: unknown task native id {fn_id}")),
        }
    }

    /// `contract_fail(msg)`: a failed `requires` blames the caller, so the
    /// message names the call site of the function that checks it.
    fn contract_failure(&mut self, ctx: *mut NativeCtx, msg: Value) -> String {
        let ptr = msg.as_ptr::<crate::memory::GcData<crate::memory::ObjString>>();
        // SAFETY: the clause's message is a live string.
        let mut text = unsafe { (*ptr).as_ref() }.to_string();
        let code = Arc::clone(&self.program.native);
        // SAFETY: the calling native code's live context.
        let sites = unsafe { &*ctx }.call_sites();
        if let Some(loc) = sites.get(1).and_then(|&s| self.locator().native_site_location(code.site(s)?)) {
            text.push_str(&format!(", called from {loc}"));
        }
        self.panic_without_location = true;
        text
    }

    fn task_scope_open(&mut self) -> Invoked {
        // Every task switches at one level: the driver runs no code nested.
        let s = self.rt.sched.get_or_insert_with(|| Box::new(crate::task::Scheduler::new(0)));
        Invoked::Value(Value::from(s.open_scope()).bits())
    }

    /// The scope body returned: wait for its children.
    fn task_scope_close(&mut self, ctx: *mut NativeCtx, id: i64) -> Invoked {
        let Some(s) = self.rt.sched.as_mut() else {
            return Invoked::Panic("task scope is not open".into());
        };
        let current = s.current;
        let Some(scope) = s.scopes.get_mut(&id) else {
            return Invoked::Panic("task scope is not open".into());
        };
        if scope.owner != current {
            return Invoked::Panic("a task scope must end in the task that opened it".into());
        }
        scope.closed = true;
        if s.children_finished(id) {
            if s.scopes.get(&id).is_some_and(|sc| sc.failed.is_some()) {
                // `task_scope_error` reads the message, then forgets the scope.
                return Invoked::Value(Value::from(crate::task::STATUS_PANICKED).bits());
            }
            s.remove_scope(id);
            self.rt.task_drop_idle_scheduler();
            return Invoked::Value(Value::from(crate::task::STATUS_OK).bits());
        }
        if !self.tasks_can_switch() {
            return Invoked::Panic("cannot wait for tasks inside a native callback".into());
        }
        self.wait(ctx, Some(crate::task::Block::Scope(id)))
    }

    /// Leaving the last `shield` delivers a cancel that waited for it.
    fn task_shield_exit(&mut self, ctx: *mut NativeCtx) -> Invoked {
        let Some(s) = self.rt.sched.as_mut() else {
            return Invoked::Value(0);
        };
        let current = s.current;
        let Some(rec) = s.tasks.get_mut(&current) else {
            return Invoked::Value(0);
        };
        rec.shield = rec.shield.saturating_sub(1);
        if current == crate::task::ROOT || rec.shield > 0 || rec.cancel != crate::task::Cancel::Requested || !self.tasks_can_switch() {
            return Invoked::Value(0);
        }
        // The scheduler switches straight back and delivers the cancel.
        self.wait(ctx, None)
    }

    /// A scheduler native's wait: its wake is the native's result.
    fn wait(&mut self, ctx: *mut NativeCtx, block: Option<crate::task::Block>) -> Invoked {
        match self.suspend(ctx, block, true) {
            Some(v) => Invoked::Value(v.bits()),
            None => Invoked::Unwound,
        }
    }

    /// Suspend the current task on `block` (`None`: ready again at once)
    /// with `ctx`'s words rooted: the wake (`0` without `result_slot`, when
    /// the native runs again), or `None` when a cancel came instead.
    #[cfg(not(target_family = "wasm"))]
    fn suspend(&mut self, ctx: *mut NativeCtx, block: Option<crate::task::Block>, result_slot: bool) -> Option<Value> {
        use crate::task::{Cancel, ROOT, TaskState, Wake};
        let s = self.rt.sched.as_mut().expect("scheduler");
        let current = s.current;
        let rec = s.tasks.get_mut(&current).expect("current task");
        let mut block = block;
        if rec.cancel == Cancel::Requested && rec.shield == 0 {
            // A cancel waits here: switch out and straight back in, which
            // delivers it.
            rec.timer_seq = None;
            s.drop_wait(current, block);
            block = None;
            s.run_queue.push_front(current);
        }
        let rec = s.tasks.get_mut(&current).expect("current task");
        rec.block = block;
        if block.is_some() {
            rec.state = TaskState::Blocked;
        } else {
            rec.state = TaskState::Ready;
            rec.wake = Some(if result_slot { Wake::Unit } else { Wake::Retry });
            if s.run_queue.front() != Some(&current) {
                s.run_queue.push_back(current);
            }
        }
        self.rt.native_roots.push((ctx, self.depth));
        let out = if current == ROOT {
            Some(self.run_tasks())
        } else {
            // The wait may be inside a generator's fiber, which stays
            // where it is: the stack bounds and yielder it runs with come
            // back with it.
            let limit = crate::stacks::FIBER_STACK_LIMIT.get();
            let gen_yielder = crate::native::GEN_YIELDER.get();
            let closing = crate::native::GEN_CLOSING.get();
            let y = TASK_YIELDER.get().cast::<corosensei::Yielder<TaskResume, ()>>();
            crate::stacks::fiber_switch_fence(std::ptr::from_mut(self));
            // SAFETY: set by the running task fiber's body.
            let resumed = unsafe { (*y).suspend(()) };
            crate::stacks::FIBER_STACK_LIMIT.set(limit);
            crate::native::GEN_YIELDER.set(gen_yielder);
            crate::native::GEN_CLOSING.set(closing);
            match resumed {
                TaskResume::Wake(v) => Some(v),
                TaskResume::Cancel => None,
            }
        };
        self.rt.native_roots.pop();
        out
    }

    #[cfg(target_family = "wasm")]
    fn suspend(&mut self, _ctx: *mut NativeCtx, _block: Option<crate::task::Block>, _result_slot: bool) -> Option<Value> {
        self.report("native driver: tasks need fibers", None, Vec::new(), false);
        None
    }

    /// The word a suspension point returns for `wake`.
    #[cfg(not(target_family = "wasm"))]
    fn wake_value(&mut self, wake: Option<crate::task::Wake>) -> Value {
        use crate::task::Wake;
        match wake {
            Some(Wake::Status(n)) => Value::from(n),
            Some(Wake::Io(r, layout)) => {
                let heap = &mut self.rt.heap;
                crate::host_enum::with_host_enum_layout(layout, || crate::io::as_result_unit(heap, r))
            }
            Some(Wake::Unit | Wake::Retry) | None => Value::default(),
        }
    }

    /// The root task waits: run the other tasks until it is ready again;
    /// its wake.
    #[cfg(not(target_family = "wasm"))]
    fn run_tasks(&mut self) -> Value {
        use crate::task::{Cancel, ROOT, TaskState};
        loop {
            let s = self.rt.sched.as_mut().expect("scheduler");
            let mut next = None;
            while let Some(id) = s.run_queue.pop_front() {
                if s.tasks.get(&id).is_some_and(|r| r.state == TaskState::Ready) {
                    next = Some(id);
                    break;
                }
            }
            let Some(id) = next else {
                if !self.rt.task_wait_events() {
                    self.rt.task_deadlock();
                }
                continue;
            };
            if id == ROOT {
                s.current = ROOT;
                let rec = s.tasks.get_mut(&ROOT).expect("root task");
                rec.state = TaskState::Running;
                let wake = rec.wake.take();
                return self.wake_value(wake);
            }
            if self.rt.task_cancel_waits_for_children(id) {
                continue;
            }
            let s = self.rt.sched.as_mut().expect("scheduler");
            s.current = id;
            let rec = s.tasks.get_mut(&id).expect("ready task");
            rec.state = TaskState::Running;
            rec.started = true;
            let wake = rec.wake.take();
            let coro = rec.coro.expect("child task");
            let input = if self.rt.task_take_cancel() { TaskResume::Cancel } else { TaskResume::Wake(self.wake_value(wake)) };
            if !self.tasks.contains_key(&id) {
                match self.new_task_fiber(coro) {
                    Some(f) => {
                        self.tasks.insert(id, f);
                    }
                    None => {
                        crate::gen_fiber::finish(coro);
                        self.rt.task_end(id, TaskState::Failed, Some("native driver: a task body without native code".into()));
                        continue;
                    }
                }
            }
            if !self.run_task_fiber(id, input) {
                continue;
            }
            // Its body returned, or unwound a panic or a cancel.
            crate::gen_fiber::finish(coro);
            let failed = std::mem::take(&mut self.panicked);
            let msg = self.task_panic.take();
            let cancelling = self.rt.sched.as_ref().and_then(|s| s.tasks.get(&id)).is_some_and(|r| r.cancel == Cancel::Unwinding);
            match (failed, msg) {
                (false, _) => self.rt.task_end(id, TaskState::Done, None),
                (true, None) if cancelling => self.rt.task_end(id, TaskState::Dropped, None),
                (true, msg) => self.rt.task_end(id, TaskState::Failed, Some(msg.unwrap_or_default())),
            }
        }
    }

    /// A fiber for task coroutine `coro`, whose body compiled.
    #[cfg(not(target_family = "wasm"))]
    fn new_task_fiber(&mut self, coro: crate::memory::RefCoroutine) -> Option<Box<TaskFiber>> {
        let c = coro.as_ref();
        let entry = self.program.native.lookup(c.resume_ip)?;
        if c.saved_stack.len() < entry.reads as usize || entry.arity as usize > 16 {
            return None;
        }
        let mut words = [0u64; 16];
        for (w, v) in words.iter_mut().zip(c.saved_stack.iter()) {
            *w = v.bits();
        }
        coro.payload_mut().started = true;
        let stack = match self.task_stacks.pop() {
            Some(s) => s,
            None => corosensei::stack::DefaultStack::new(crate::stacks::FIBER_STACK_BYTES).ok()?,
        };
        use corosensei::stack::Stack as _;
        let stack_limit = stack.limit().get() + 64 * 1024;
        let me = std::ptr::from_mut(self).cast::<c_void>();
        let coro = TaskCoro::with_stack(stack, move |yielder, _first| {
            // SAFETY: the driver outlives its task fibers (it drops them).
            let d = unsafe { &mut *me.cast::<Driver<'static>>() };
            TASK_YIELDER.set(std::ptr::from_ref(yielder).cast());
            let env = d.env();
            let out = crate::native::call(&entry, &words[..entry.arity as usize], &env);
            d.task_body_done(out);
        });
        Some(Box::new(TaskFiber { coro: Some(coro), stack_limit, yielder: std::ptr::null(), native_roots: Vec::new(), arg_roots: Vec::new(), depth: 0, gen_running: Vec::new() }))
    }

    /// A task body's outcome, on its fiber: what it frees, or its panic
    /// (which fails the task) and the `defer`s it unwound.
    #[cfg(not(target_family = "wasm"))]
    fn task_body_done(&mut self, out: NativeOutcome) {
        match out {
            NativeOutcome::Done(r) => {
                for (w, deep) in r.frees {
                    if !self.free_early(Value::from(w), deep) {
                        return;
                    }
                }
            }
            NativeOutcome::Panicked(p) => self.native_panic(p),
            NativeOutcome::Bailed => self.report("native driver: a task could not start", None, Vec::new(), false),
        }
    }

    /// Run task `id`'s fiber with `input` until it waits (`false`) or its
    /// body is done (`true`).
    #[cfg(not(target_family = "wasm"))]
    fn run_task_fiber(&mut self, id: crate::task::TaskId, input: TaskResume) -> bool {
        let f: *mut TaskFiber = &mut **self.tasks.get_mut(&id).expect("a task fiber");
        // SAFETY: the box stays in the map while it runs; only this loop
        // removes fibers.
        let f = unsafe { &mut *f };
        self.swap_roots(f);
        let saved_limit = crate::stacks::FIBER_STACK_LIMIT.replace(f.stack_limit);
        let saved_gen = crate::native::GEN_YIELDER.get();
        let saved_yielder = TASK_YIELDER.replace(f.yielder);
        crate::stacks::fiber_switch_fence(std::ptr::from_mut(self));
        let out = f.coro.as_mut().expect("a parked task fiber").resume(input);
        f.yielder = TASK_YIELDER.replace(saved_yielder);
        crate::native::GEN_YIELDER.set(saved_gen);
        crate::stacks::FIBER_STACK_LIMIT.set(saved_limit);
        self.swap_roots(f);
        match out {
            corosensei::CoroutineResult::Yield(()) => false,
            corosensei::CoroutineResult::Return(()) => {
                let mut f = self.tasks.remove(&id).expect("a task fiber");
                if let Some(coro) = f.coro.take()
                    && self.task_stacks.len() < 8
                {
                    self.task_stacks.push(coro.into_stack());
                }
                true
            }
        }
    }

    /// Trade the running side's native calls, argument roots, call depth
    /// and running generators with `f`'s.
    #[cfg(not(target_family = "wasm"))]
    fn swap_roots(&mut self, f: &mut TaskFiber) {
        std::mem::swap(&mut self.rt.native_roots, &mut f.native_roots);
        std::mem::swap(&mut self.rt.ffi_arg_roots, &mut f.arg_roots);
        std::mem::swap(&mut self.depth, &mut f.depth);
        std::mem::swap(&mut self.gen_running, &mut f.gen_running);
    }
}

impl Drop for Driver<'_> {
    fn drop(&mut self) {
        // Tasks still waiting (the root task panicked): forget them.
        #[cfg(not(target_family = "wasm"))]
        if !self.tasks.is_empty() {
            self.tasks.clear();
            self.rt.sched = None;
        }
    }
}

impl GcHost for Driver<'_> {
    fn rt(&self) -> &Runtime {
        self.rt
    }

    fn rt_mut(&mut self) -> &mut Runtime {
        self.rt
    }

    /// The generators whose fibers run now: native frames' records and
    /// the Runtime's roots are the rest.
    fn host_roots(&self, visit: &mut dyn FnMut(HostRoot)) {
        use crate::memory::RootKind;
        for v in &self.gen_running {
            visit(HostRoot::Handle(v.heap_addr(), RootKind::Ambiguous));
        }
        // The task side not running: its native frames' words and argument
        // roots, pinned.
        #[cfg(not(target_family = "wasm"))]
        for f in self.tasks.values() {
            for v in f.arg_roots.iter().chain(&f.gen_running) {
                visit(HostRoot::Handle(v.heap_addr(), RootKind::Ambiguous));
            }
            for &(ctx, _) in &f.native_roots {
                // SAFETY: a parked call's frames keep their contexts.
                unsafe { &*ctx }.for_each_root(|w, _| visit(HostRoot::Handle(Value::from(unsafe { *w }).heap_addr(), RootKind::Ambiguous)));
            }
        }
    }

    fn slot(&self, _idx: usize) -> Value {
        Value::default()
    }

    fn set_slot(&mut self, _idx: usize, _v: Value) {}

    /// Not while auto-par workers run on the heap.
    fn can_move(&self) -> bool {
        self.rt.shared_epoch.is_none()
    }

    fn run_pending(&mut self, val: Value, pc: Option<u32>) {
        let Some(pc) = pc else {
            return self.close_generator(val);
        };
        if self.finalizer_aborted || !self.rt.claim_finalizer(val) {
            return;
        }
        let was = std::mem::replace(&mut self.panicked, false);
        let n = self.rt.ffi_arg_roots.len();
        self.rt.ffi_arg_roots.push(val);
        let _ = self.call(pc as usize, &[val]);
        self.rt.ffi_arg_roots.truncate(n);
        if self.panicked {
            self.abort_after_finalizer_panic();
            return;
        }
        self.panicked = was;
    }
}

impl crate::gen_fiber::GenHost for Driver<'_> {
    fn rt(&mut self) -> &mut Runtime {
        self.rt
    }

    #[cfg(not(target_family = "wasm"))]
    fn gen_stacks(&mut self) -> &mut Vec<corosensei::stack::DefaultStack> {
        &mut self.gen_stacks
    }

    fn gen_running(&mut self) -> &mut Vec<Value> {
        &mut self.gen_running
    }

    fn native_env(&mut self) -> NativeEnv {
        self.env()
    }

    fn free_early(&mut self, v: Value, deep: bool) {
        let _ = Driver::free_early(self, v, deep);
    }

    fn switches_in_fibers(&self) -> bool {
        true
    }
}

impl HostEnv for Driver<'_> {
    fn rt(&mut self) -> &mut Runtime {
        self.rt
    }

    fn frame_depth(&self) -> usize {
        self.depth
    }

    fn can_switch(&self) -> bool {
        self.tasks_can_switch()
    }

    /// The root task waits where it is (the scheduler runs there); a
    /// child parks its fiber.
    fn on_fiber(&self) -> bool {
        true
    }

    fn park(&mut self, ctx: *mut NativeCtx, block: Option<crate::task::Block>, result_slot: bool) -> Option<Value> {
        self.suspend(ctx, block, result_slot)
    }

    fn collect(&mut self, ctx: *mut NativeCtx) -> bool {
        self.rooted(ctx, crate::collect::collect);
        self.panicked
    }
}
