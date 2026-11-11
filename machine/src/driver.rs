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

use common::{ProgramDebug, Value};

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

/// A child task parked at a wait ([`crate::native::parks`]).
struct ParkedTask {
    call: crate::native::ParkedCall,
    /// Its op runs again on the wake, instead of taking it as its result.
    retry: bool,
}

/// What a task is resumed with: its wait's wake, or a cancel.
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

/// The program a driver runs: its code (for the prologue and the
/// snapshot pool), the native code for it and its debug tables.
pub(crate) struct DriverProgram {
    pub(crate) startup: Arc<Vec<common::StartupOp>>,
    pub(crate) main: Option<u32>,
    pub(crate) constants: Arc<Vec<u64>>,
    pub(crate) native: Arc<NativeCode>,
    /// Shared, so a driver per call ([`crate::Isolate::call`]) does not
    /// copy the program's tables.
    pub(crate) debug: Arc<ProgramDebug>,
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
    /// Panics stop the run and are not printed: a speculative job's
    /// ([`crate::reactor::Job::speculative`]).
    quiet: bool,
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
    /// Child tasks' parked calls, by task id ([`crate::native::parks`]).
    parked_tasks: std::collections::HashMap<crate::task::TaskId, ParkedTask>,
    /// A parking wait of the running task: whether its op runs again.
    park_retry: bool,
    /// The wake of a wait inside a generator, on its way down to it.
    pending_wake: Option<(u64, u64)>,
    /// The generator just resumed parked for its task's wait.
    gen_waiting: bool,
    /// Cleanup pads running: a task unwinding has no frames left to park, so
    /// a wait in one runs the other tasks from here.
    pads_running: usize,
    /// `depth` where the running task's body runs, parked tasks' way:
    /// anything above it has host frames the task cannot leave behind.
    /// Generators do not count: a generator parks with what resumed it.
    task_base: usize,
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
        tasks: Driver::host_tasks,
        apply: Driver::host_apply,
        call_value: Driver::host_call_value,
        claim: Driver::host_claim,
        resume: Driver::host_resume,
        collect: Driver::host_collect,
        gc: Driver::host_gc,
        free: Driver::host_free,
        ffi: Driver::host_ffi,
        invoke: Driver::host_invoke,
    };

    pub(crate) fn new(rt: &'a mut Runtime, program: DriverProgram) -> Self {
        Self {
            rt,
            program,
            panicked: false,
            quiet: false,
            finalizer_aborted: false,
            depth: 0,
            #[cfg(not(target_family = "wasm"))]
            gen_stacks: Vec::new(),
            gen_running: Vec::new(),
            parked_tasks: std::collections::HashMap::new(),
            park_retry: false,
            pending_wake: None,
            gen_waiting: false,
            pads_running: 0,
            task_base: 0,
            #[cfg(not(target_family = "wasm"))]
            tasks: std::collections::HashMap::new(),
            #[cfg(not(target_family = "wasm"))]
            task_stacks: Vec::new(),
            task_panic: None,
            panic_without_location: false,
            panic_message: String::new(),
        }
    }

    /// Whether this run keeps its panics to itself: a speculative job does.
    pub(crate) fn set_quiet(&mut self, quiet: bool) {
        self.quiet = quiet;
    }

    /// [`NativeHost::apply`].
    unsafe fn host_apply(raw: *mut c_void, callee: u64, args: &[Value], dicts: usize) -> crate::native::Applied {
        use crate::native::Applied;
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let code_len = d.program.native.code_len();
        match crate::apply::apply(&mut d.rt.heap, Value::from(callee), args, dicts, code_len) {
            crate::apply::Apply::Value(v) => Applied::Value(v.bits()),
            crate::apply::Apply::Call { pc, args, extras } if extras.is_empty() => Applied::Call { pc, args },
            _ => Applied::Other,
        }
    }

    /// [`NativeHost::tasks`].
    unsafe fn host_tasks(raw: *mut c_void) -> crate::native::DebugTasks {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        d.rt.sched.as_ref().map_or((crate::task::ROOT, Vec::new()), |s| (s.current, s.debug_states()))
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

    /// Whether `native` covers `program`: its startup and its `main`.
    pub(crate) fn covers(startup: &[common::StartupOp], main: Option<u32>, native: &NativeCode) -> bool {
        use common::StartupOp as Op;
        native.is_complete()
            && main.is_none_or(|m| native.lookup(m as usize).is_some())
            && startup.iter().all(|op| match *op {
                Op::Call { entry } | Op::Init { entry, .. } => native.lookup(entry as usize).is_some(),
                Op::Restore { .. } => true,
            })
    }

    /// What runs before `main`. A panic stops it.
    pub(crate) fn run_startup(&mut self) {
        use common::StartupOp as Op;
        let ops = Arc::clone(&self.program.startup);
        for op in ops.iter() {
            if self.panicked {
                return;
            }
            match *op {
                Op::Call { entry } => {
                    let _ = self.call(entry as usize, &[]);
                }
                Op::Init { entry, slot } => {
                    let word = self.call(entry as usize, &[]).map_or_else(Value::default, |(lo, _)| Value::from(lo));
                    if let Some(s) = self.rt.statics.get_mut(slot as usize) {
                        *s = word;
                    }
                }
                Op::Restore { at } => self.restore_snapshot(at as usize),
            }
        }
    }

    /// The whole program: its startup, then `main`.
    pub(crate) fn run_program(&mut self) {
        self.run_startup();
        if let Some(main) = self.program.main.filter(|_| !self.panicked) {
            let _ = self.call(main as usize, &[]);
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
            probes: &raw mut self.rt.probes,
            code: Arc::as_ptr(&self.program.native),
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
        if args.len() < entry.reads as usize {
            self.report("native driver: a call with too few words", None, Vec::new(), false);
            return None;
        }
        let mut few = [0u64; 16];
        let mut many = Vec::new();
        let words: &mut [u64] = if params <= few.len() {
            &mut few[..params]
        } else {
            many.resize(params, 0);
            &mut many
        };
        for (w, a) in words.iter_mut().zip(args.iter().take(params)) {
            *w = a.bits();
        }
        if let Some(t) = self.rt.native.as_mut() {
            t.hits += 1;
        }
        let env = self.env();
        self.depth += 1;
        #[cfg(not(target_family = "wasm"))]
        let out = crate::stacks::on_deep_stack(|| crate::native::call(&entry, words, &env));
        #[cfg(target_family = "wasm")]
        let out = crate::native::call(&entry, words, &env);
        self.depth -= 1;
        match out {
            NativeOutcome::Done(r) => {
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
            // Only a task body or a generator parks, and neither comes here.
            NativeOutcome::Parked(..) => {
                self.report("native driver: a call parked with native frames under it", None, Vec::new(), false);
                None
            }
        }
    }

    /// Call function word `callee` on `args`, the last `dicts` of them
    /// instance dictionaries, as `CallIndirect` does.
    pub(crate) fn call_value(&mut self, callee: Value, args: &[Value], dicts: usize) -> Option<Value> {
        let (mut callee, mut args, mut dicts) = (callee, args.to_vec(), dicts);
        loop {
            let code_len = self.program.native.code_len();
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
            Some(crate::gen_fiber::GenStep::Waiting) => {
                self.gen_waiting = true;
                None
            }
            None => {
                // A body with no `yield` compiles as a plain function: the
                // first resume runs it to its return.
                let c = gc.as_ref();
                let plain = !c.started && c.state == crate::memory::CoroState::Suspended;
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
        let pads = crate::gen_fiber::close(self, gc);
        crate::gen_fiber::finish(gc);
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
        if let Some(pads) = crate::gen_fiber::close(self, gc) {
            let was = std::mem::replace(&mut self.panicked, false);
            self.run_pads(pads);
            if self.panicked {
                self.abort_after_finalizer_panic();
                return;
            }
            self.panicked = was;
        }
    }

    /// A native panic, unless it is one a nested call already reported
    /// ([`crate::native::TRAP_PANICKED`]); then its frames' pads run.
    fn native_panic(&mut self, p: NativePanic) {
        if p.code != crate::native::TRAP_PANICKED {
            let code = Arc::clone(&self.program.native);
            let locator = self.locator();
            // A stack overflow traps in the prologue of a frame that never
            // started: the panic is at the call that made it.
            let skip = usize::from(p.code == crate::native::TRAP_STACK);
            let mut sites: Vec<u32> = p.trace.iter().skip(skip).copied().collect();
            let loc = sites.first().and_then(|&s| locator.native_site_location(code.site(s)?));
            // The native calls under this one, innermost first.
            for &(ctx, _) in self.rt.native_roots.iter().rev() {
                // SAFETY: a native call in progress keeps its context alive.
                sites.extend(unsafe { &*ctx }.call_sites());
            }
            let frames = locator.backtrace(&code, sites);
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
        let mut sites = unsafe { &*ctx }.call_sites();
        let loc = sites.first().and_then(|&s| locator.native_site_location(code.site(s)?));
        for &(c, _) in self.rt.native_roots.iter().rev() {
            // SAFETY: a native call in progress keeps its context alive.
            sites.extend(unsafe { &*c }.call_sites());
        }
        let frames = locator.backtrace(&code, sites);
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
        self.pads_running += 1;
        crate::native::run_pads(pads, &env, &mut |p| raised.push(p));
        self.pads_running -= 1;
        for p in raised {
            self.native_panic(p);
        }
    }

    /// A panic of this run: printed ([`Self::print_panic`]), then the
    /// fork-join jobs it left out are joined.
    fn report(&mut self, message: &str, loc: Option<String>, frames: Vec<String>, explicit: bool) {
        self.print_panic(message, loc, frames, explicit);
        self.settle_speculative();
    }

    /// Cancel and join the fork-join jobs this run has out. A panic
    /// unwinds a fork-join past its joins (a recursive fork-join runs
    /// arms on its own thread), and its jobs would run on while the heap
    /// is used or dropped under them, with the steal epoch they hold left
    /// open.
    fn settle_speculative(&mut self) {
        let out = std::mem::take(&mut *self.rt.speculative_out.lock().unwrap_or_else(|e| e.into_inner()));
        // Nothing will use what they compute: stop them first (#2442).
        for job in &out {
            job.cancel();
        }
        for job in out {
            crate::thread::join_abandoned(self, &job);
        }
    }

    /// Print a panic as the runtime does: `panic: message at loc`, then the
    /// backtrace (an explicit `panic` only under `COIL_BACKTRACE`).
    fn print_panic(&mut self, message: &str, loc: Option<String>, frames: Vec<String>, explicit: bool) {
        use std::io::Write;
        self.panicked = true;
        self.panic_message = message.to_string();
        let without_location = std::mem::take(&mut self.panic_without_location);
        if self.rt.sched.as_ref().is_some_and(|s| s.current != crate::task::ROOT) {
            // A child task's panic fails its scope, which reports it.
            self.task_panic = Some(message.to_string());
            return;
        }
        if self.quiet {
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
        crate::locate::Locator { debug: &self.program.debug }
    }

    /// `--cow-report`: the copies copy-on-write made, by source line, most
    /// first, on stderr.
    pub(crate) fn print_cow_report(&mut self) {
        let Some(sites) = self.rt.heap.get_mut().take_cow_sites() else { return };
        let code = Arc::clone(&self.program.native);
        let locator = self.locator();
        let mut by_line: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for (s, n) in sites {
            let at = code.site(s).and_then(|site| locator.native_site_location(site)).unwrap_or_else(|| format!("site {s}"));
            // The report is by line.
            let at = at.rsplit_once(':').filter(|(line, _)| line.contains(':')).map_or(at.clone(), |(line, _)| line.to_string());
            *by_line.entry(at).or_insert(0) += n;
        }
        let mut rows: Vec<(String, u64)> = by_line.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let total: u64 = rows.iter().map(|r| r.1).sum();
        let heap = self.rt.heap.get();
        eprintln!(
            "cow-report: {total} copies for {} shares ({} objects copied; each share is a copy a copy-on-assign design makes)",
            heap.cow_shares(),
            heap.cow_copies()
        );
        // A line that copies over and over (a write in a loop to a value
        // something else shares) is worth a borrow or a `Ref<T>`.
        const HOT: u64 = 100;
        for (at, n) in &rows {
            let hint = if *n >= HOT { "  <- copied repeatedly: borrow with `let x = mut …` or share through a `Ref<T>`" } else { "" };
            eprintln!("  {n:>10}  {at}{hint}");
        }
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
        if std::mem::take(&mut d.gen_waiting) {
            // SAFETY: the running call's context.
            unsafe { (*ctx).park = crate::native::PARK_BELOW };
        }
        *panicked = out.is_none();
        out.unwrap_or_default().bits()
    }

    /// Run `f`, telling whether it panicked itself: a host call from a
    /// `defer` pad that runs while a panic unwinds starts with `panicked`
    /// already set, and that panic is not the call's (#2312).
    fn fresh_panic<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> (R, bool) {
        let was = std::mem::replace(&mut self.panicked, false);
        let out = f(self);
        let fresh = self.panicked;
        self.panicked |= was;
        (out, fresh)
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
        let ((), fresh) = d.fresh_panic(|d| d.rooted(ctx, crate::collect::collect));
        *panicked = fresh;
        before.saturating_sub(d.rt.heap.size()) as u64
    }

    unsafe fn host_free(raw: *mut c_void, ctx: *mut NativeCtx, word: u64, deep: bool) -> bool {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let (freed, fresh) = d.fresh_panic(|d| d.rooted(ctx, |d| d.free_early(Value::from(word), deep)));
        freed && !fresh
    }

    unsafe fn host_ffi(raw: *mut c_void, ctx: *mut NativeCtx, op: u64, operand: u64, args: &[Value], panicked: &mut bool) -> Option<u64> {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        let arg = |i: usize| args.get(i).copied().unwrap_or_default();
        let was = std::mem::replace(&mut d.panicked, false);
        let out = match op as u8 {
            common::FFI_OP_LOAD => d.rt.ffi_load(arg(0)),
            common::FFI_OP_DECLARE => d.rt.ffi_declare(operand as u32, arg(0), arg(1), arg(2), arg(3)),
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
        d.panicked |= was;
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
        match d.rt.natives.get_by_id(fn_id)?.host_op() {
            crate::HostOp::Ordinary => crate::runtime::invoke_ordinary(d, ctx, fn_id, layout, args),
            crate::HostOp::RegisterFinalizer => {
                let word = |i: usize| args.get(i).map_or(0, |v| v.as_int() as u32);
                d.rt.register_finalizer(word(0), word(1));
                Some(Invoked::Value(0))
            }
            op @ (crate::HostOp::RegionEnter | crate::HostOp::RegionExit) => {
                let ((), fresh) = d.fresh_panic(|d| d.rooted(ctx, |d| crate::collect::region(d, op == crate::HostOp::RegionEnter)));
                Some(if fresh { Invoked::Unwound } else { Invoked::Value(0) })
            }
            crate::HostOp::Task if fn_id == common::TASK_SCOPE_OPEN_ID as usize + GEN_CLOSE_OP => {
                let handle = args.first().copied().unwrap_or_default();
                Some(if d.rooted(ctx, |d| d.close_now(handle)) { Invoked::Value(0) } else { Invoked::Unwound })
            }
            crate::HostOp::Task => Some(d.task_op(ctx, fn_id, args)),
            crate::HostOp::Restore => {
                let at = args.first().map_or(0, |v| v.as_int() as usize);
                d.rooted(ctx, |d| d.restore_snapshot(at));
                Some(if d.panicked { Invoked::Unwound } else { Invoked::Value(0) })
            }
            _ => Some(Invoked::Panic("native driver: unknown host op".into())),
        }
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
    /// message names the call site of the function that checks it, through
    /// a dictionary call as through a direct one.
    fn contract_failure(&mut self, ctx: *mut NativeCtx, msg: Value) -> String {
        let ptr = msg.as_ptr::<crate::memory::GcData<crate::memory::ObjString>>();
        // SAFETY: the clause's message is a live string.
        let mut text = unsafe { (*ptr).as_ref() }.to_string();
        let code = Arc::clone(&self.program.native);
        // SAFETY: the calling native code's live context.
        let sites = unsafe { &*ctx }.call_sites();
        // The caller is the first frame below with a source location: a
        // dictionary's adapter between the two is compiler-made and has
        // none (#2503).
        if let Some(loc) = sites.iter().skip(1).find_map(|&s| self.locator().native_site_location(code.site(s)?)) {
            text.push_str(&format!(", called from {loc}"));
        }
        self.panic_without_location = true;
        text
    }

    fn task_scope_open(&mut self) -> Invoked {
        // Every task switches at one level: the driver runs no code nested.
        let s = self.rt.sched.get_or_insert_with(|| Box::new(crate::task::Scheduler::new()));
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
    fn suspend(&mut self, ctx: *mut NativeCtx, block: Option<crate::task::Block>, result_slot: bool) -> Option<Value> {
        use crate::task::{Cancel, ROOT, TaskState, Wake};
        let parks = crate::native::parks() && self.pads_running == 0;
        let nested = crate::native::parks() && self.pads_running != 0;
        let s = self.rt.sched.as_mut().expect("scheduler");
        let current = s.current;
        if parks && current != ROOT && self.depth != self.task_base {
            self.report("a task cannot wait inside a native callback here (no fibers)", None, Vec::new(), false);
            return None;
        }
        let s = self.rt.sched.as_mut().expect("scheduler");
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
        if parks && current != ROOT {
            // The call parks: the host op returns, and the interpreter
            // keeps its frames for the wake.
            // SAFETY: the calling native code's live context.
            unsafe {
                (*ctx).park = crate::native::PARK_WAIT;
                (*ctx).park_word = 0;
            }
            self.park_retry = !result_slot;
            return None;
        }
        self.rt.native_roots.push((ctx, self.depth));
        let out = if current == ROOT || nested {
            Some(self.run_tasks(current))
        } else {
            self.fiber_wait()
        };
        self.rt.native_roots.pop();
        out
    }

    /// Switch from the running task's fiber to the root's; its wake.
    #[cfg(target_family = "wasm")]
    fn fiber_wait(&mut self) -> Option<Value> {
        unreachable!("no fibers: a task parks")
    }

    /// Switch from the running task's fiber to the root's; its wake.
    #[cfg(not(target_family = "wasm"))]
    fn fiber_wait(&mut self) -> Option<Value> {
        {
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
        }
    }

    /// The word a suspension point returns for `wake`.
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

    /// Run task `id` until it waits (`Some(false)`) or its body is done
    /// (`Some(true)`). `None`: it has no body to run.
    fn run_task(&mut self, id: crate::task::TaskId, coro: crate::memory::RefCoroutine, input: TaskResume) -> Option<bool> {
        if crate::native::parks() {
            return self.run_parked_task(id, coro, input);
        }
        #[cfg(not(target_family = "wasm"))]
        {
            if !self.tasks.contains_key(&id) {
                let fiber = self.new_task_fiber(coro)?;
                self.tasks.insert(id, fiber);
            }
            Some(self.run_task_fiber(id, input))
        }
        #[cfg(target_family = "wasm")]
        unreachable!("no fibers: a task parks")
    }

    /// [`Self::run_task`] where tasks park.
    fn run_parked_task(&mut self, id: crate::task::TaskId, coro: crate::memory::RefCoroutine, input: TaskResume) -> Option<bool> {
        use crate::native::{RESUME_CANCEL, RESUME_RETRY, RESUME_VALUE};
        let parked = self.parked_tasks.remove(&id);
        let start = if parked.is_some() {
            None
        } else {
            let c = coro.as_ref();
            let entry = self.program.native.lookup(c.resume_ip)?;
            if c.saved_stack.len() < entry.reads as usize {
                return None;
            }
            let mut words = vec![0u64; entry.arity as usize];
            for (w, v) in words.iter_mut().zip(c.saved_stack.iter()) {
                *w = v.bits();
            }
            coro.payload_mut().started = true;
            Some((entry, words))
        };
        let base = std::mem::replace(&mut self.task_base, self.depth);
        let env = self.env();
        let out = match parked {
            Some(t) => {
                let (word, how) = match input {
                    TaskResume::Cancel => (0, RESUME_CANCEL),
                    TaskResume::Wake(_) if t.retry => (0, RESUME_RETRY),
                    TaskResume::Wake(v) => (v.bits(), RESUME_VALUE),
                };
                if t.call.kind == crate::native::PARK_BELOW {
                    // The wait is in a generator: its resume runs again.
                    self.pending_wake = Some((word, how));
                    t.call.resume(0, RESUME_RETRY, &env)
                } else {
                    t.call.resume(word, how, &env)
                }
            }
            None => {
                let (entry, words) = start.expect("a new task's call");
                crate::native::call(&entry, &words, &env)
            }
        };
        if let NativeOutcome::Parked(call, _) = out {
            self.parked_tasks.insert(id, ParkedTask { call, retry: self.park_retry });
            self.task_base = base;
            return Some(false);
        }
        self.task_body_done(out);
        self.task_base = base;
        Some(true)
    }

    /// Task `until` waits on the stack, the root's or a `defer`'s: run the
    /// others until it is ready again; its wake. The root stands below a
    /// nested wait and cannot run under it.
    fn run_tasks(&mut self, until: crate::task::TaskId) -> Value {
        use crate::task::{Cancel, ROOT, TaskState};
        loop {
            let s = self.rt.sched.as_mut().expect("scheduler");
            let mut next = None;
            let mut root_ready = false;
            while let Some(id) = s.run_queue.pop_front() {
                if s.tasks.get(&id).is_some_and(|r| r.state == TaskState::Ready) {
                    if id == ROOT && until != ROOT {
                        root_ready = true;
                        continue;
                    }
                    next = Some(id);
                    break;
                }
            }
            if root_ready {
                s.run_queue.push_back(ROOT);
            }
            let Some(id) = next else {
                if !self.rt.task_wait_events() {
                    self.rt.task_deadlock();
                }
                continue;
            };
            if id == until {
                s.current = until;
                let rec = s.tasks.get_mut(&until).expect("waiting task");
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
            match self.run_task(id, coro, input) {
                Some(false) => continue,
                Some(true) => {}
                None => {
                    crate::gen_fiber::finish(coro);
                    self.rt.task_end(id, TaskState::Failed, Some("native driver: a task body without native code".into()));
                    continue;
                }
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
        if c.saved_stack.len() < entry.reads as usize {
            return None;
        }
        let mut words = vec![0u64; entry.arity as usize];
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
            let out = crate::native::call(&entry, &words, &env);
            d.task_body_done(out);
        });
        Some(Box::new(TaskFiber { coro: Some(coro), stack_limit, yielder: std::ptr::null(), native_roots: Vec::new(), arg_roots: Vec::new(), depth: 0, gen_running: Vec::new() }))
    }

    /// A task body's outcome, on its fiber: what it frees, or its panic
    /// (which fails the task) and the `defer`s it unwound.
    fn task_body_done(&mut self, out: NativeOutcome) {
        match out {
            NativeOutcome::Done(_) => {}
            NativeOutcome::Panicked(p) => self.native_panic(p),
            NativeOutcome::Parked(..) => self.report("native driver: a task could not start", None, Vec::new(), false),
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
            visit(HostRoot(v.heap_addr(), RootKind::Ambiguous));
        }
        // Parked tasks' frames, pinned.
        for t in self.parked_tasks.values() {
            t.call.for_each_word(|w| visit(HostRoot(Value::from(w).heap_addr(), RootKind::Ambiguous)));
        }
        // The task side not running: its native frames' words and argument
        // roots, pinned.
        #[cfg(not(target_family = "wasm"))]
        for f in self.tasks.values() {
            for v in f.arg_roots.iter().chain(&f.gen_running) {
                visit(HostRoot(v.heap_addr(), RootKind::Ambiguous));
            }
            for &(ctx, _) in &f.native_roots {
                // SAFETY: a parked call's frames keep their contexts.
                unsafe { &*ctx }.for_each_root(|w, _| visit(HostRoot(Value::from(unsafe { *w }).heap_addr(), RootKind::Ambiguous)));
            }
        }
    }

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

    fn pending_wake(&mut self) -> Option<&mut Option<(u64, u64)>> {
        Some(&mut self.pending_wake)
    }

    fn native_env(&mut self) -> NativeEnv {
        self.env()
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
