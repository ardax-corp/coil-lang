//! A program run by its native code alone, without the interpreter
//! (`docs/internals/native.md`, "The native driver"). The driver owns the
//! [`Runtime`], walks the prologue's startup (`RestoreSnapshot`, the
//! static initializers and `__module_setup`, which are native calls) and
//! calls `main` or a test case natively. Native code reaches the driver
//! through its [`NativeHost`]: calls of other bodies and of function
//! values run natively, host natives on the Runtime, collections over the
//! native frames' root records ([`GcHost`]).
//!
//! Not yet: tasks, generators and FFI callbacks (each reports a panic
//! naming the gap).

use std::ffi::c_void;
use std::sync::Arc;

use common::{Instruction, ProgramDebug, Value};

use crate::collect::{GcHost, HostRoot};
use crate::native::{Invoked, NativeCode, NativeCtx, NativeEnv, NativeHost, NativeOutcome, NativePanic};
use crate::runtime::{HostEnv, Runtime};

/// The program a driver runs: its code (for the prologue and the
/// snapshot pool), the native code for it and its debug tables.
pub(crate) struct DriverProgram {
    pub(crate) code: Arc<Vec<common::Byte>>,
    pub(crate) constants: Arc<Vec<u64>>,
    pub(crate) native: Arc<NativeCode>,
    pub(crate) debug: ProgramDebug,
    pub(crate) base_dir: Option<std::path::PathBuf>,
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
        Self { rt, program, panicked: false, finalizer_aborted: false, depth: 0 }
    }

    pub(crate) fn panicked(&self) -> bool {
        self.panicked
    }

    /// Whether the driver can run the program that starts at `ip`: its
    /// prologue holds only startup ops, and every body it calls compiled.
    pub(crate) fn can_start(code: &[common::Byte], native: &NativeCode, ip: usize) -> bool {
        let mut pc = ip;
        for _ in 0..code.len() {
            let Some(b) = code.get(pc) else { return false };
            match *b.bytecode() {
                Instruction::CALL => {
                    if native.lookup(b.call_parts().1).is_none() {
                        return false;
                    }
                }
                Instruction::StoreStatic | Instruction::POP | Instruction::NOOP | Instruction::RestoreSnapshot => {}
                Instruction::HALT => return true,
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
        let env = self.env();
        self.depth += 1;
        #[cfg(not(target_family = "wasm"))]
        let out = crate::vm::on_deep_stack(|| crate::native::call(&entry, &words[..params], &env));
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
            self.report(p.message(), loc, frames, explicit);
        }
        self.panicked = true;
        self.run_pads(p.pads);
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

    unsafe fn host_resume(raw: *mut c_void, _ctx: *mut NativeCtx, _handle: u64, _send: Option<u64>, panicked: &mut bool) -> u64 {
        // SAFETY: native code passes back the driver it was given.
        let d = unsafe { Self::from_raw(raw) };
        d.report("native driver: generators are not supported yet", None, Vec::new(), false);
        *panicked = true;
        0
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
                let call = crate::vm::FfiCall::from_words(&d.rt.heap, args);
                d.rooted(ctx, |d| {
                    let mut callback = |_| Err(crate::ffi::FfiError::Panic("native driver: FFI callbacks are not supported yet".into()));
                    d.rt.ffi_invoke(call.lib_addr, call.function_id, &call.args, call.arg_types.as_deref(), &mut callback)
                })
            }
        };
        *panicked = d.panicked;
        Some(out.bits())
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
            _ => Some(Invoked::Panic("native driver: tasks are not supported yet".into())),
        };
        if out.is_some() {
            crate::native::note_host_call(fn_id, crate::native::HostPath::Runtime);
        }
        out
    }
}

impl GcHost for Driver<'_> {
    fn rt(&self) -> &Runtime {
        self.rt
    }

    fn rt_mut(&mut self) -> &mut Runtime {
        self.rt
    }

    /// The driver holds no words of its own: native frames' records and
    /// the Runtime's roots are all.
    fn host_roots(&self, _visit: &mut dyn FnMut(HostRoot)) {}

    fn slot(&self, _idx: usize) -> Value {
        Value::default()
    }

    fn set_slot(&mut self, _idx: usize, _v: Value) {}

    fn can_move(&self) -> bool {
        true
    }

    fn run_pending(&mut self, val: Value, pc: Option<u32>) {
        let Some(pc) = pc else {
            // A generator's close: none runs natively yet.
            return;
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

impl HostEnv for Driver<'_> {
    fn rt(&mut self) -> &mut Runtime {
        self.rt
    }

    fn frame_depth(&self) -> usize {
        self.depth
    }

    fn can_switch(&self) -> bool {
        false
    }

    fn on_fiber(&self) -> bool {
        false
    }

    fn park(&mut self, _ctx: *mut NativeCtx, _block: Option<crate::task::Block>, _result_slot: bool) -> Option<Value> {
        None
    }

    fn collect(&mut self, ctx: *mut NativeCtx) -> bool {
        self.rooted(ctx, crate::collect::collect);
        self.panicked
    }
}
