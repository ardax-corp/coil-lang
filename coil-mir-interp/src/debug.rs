//! Stops for a debugger: the interpreter's half of `coil debug`.
//!
//! The program runs on a thread of its own, on the `DEBUG` instantiation of
//! the interpreter's loop, which comes here before each op. At a stop that
//! thread waits, with its frames as they are, and the debugger reads them; it
//! makes no difference how deep native code nested activations, or that a
//! task or generator runs on a fiber.
//!
//! Ops are numbered across the program ([`prepare_debug`]). The numbers stand
//! where entry ids stood: breakpoints, the step modes of
//! [`DebugController`], and a [`common::ProgramDebug`] built over them. A
//! local's value is the word of the value last bound to it
//! (`MirFunc::local_binds`), which is exact on raw MIR: registers stay put
//! and every value keeps its own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use common::{DebugLoc, Value};
use compiler::native_mir::{MirFunc, NativeBody, ValueId};
use machine::native::{self as rt, NativeCtx, TRAP_CLOSE, TRAP_PARKED, TRAP_PANICKED};
use machine::{DebugController, DebugObject, DebugTask, DebugTaskFrames, StopReason};

use crate::frame::{AT_BLOCK_START, Activation};
use crate::{Body, InterpProgram, reg};

/// A body's ops by number.
pub(crate) struct BodyOps {
    /// Each block's first op.
    blocks: Vec<u32>,
    /// The locals each value binds.
    binds: HashMap<ValueId, Vec<u32>>,
}

impl BodyOps {
    /// Number `f`'s ops from `next`, appending their locations to `locs`.
    pub(crate) fn number(f: &MirFunc, locs: &mut Vec<DebugLoc>) -> Self {
        let mut blocks = Vec::with_capacity(f.blocks.len());
        for b in &f.blocks {
            blocks.push(locs.len() as u32);
            for inst in &b.insts {
                let dest = inst.dest();
                let loc = f.op_locs.get(&dest).or_else(|| f.value_locs.get(&dest));
                locs.push(loc.copied().unwrap_or_else(DebugLoc::unknown));
            }
            locs.push(f.term_locs.get(&b.id).copied().unwrap_or_else(DebugLoc::unknown));
        }
        let mut binds: HashMap<ValueId, Vec<u32>> = HashMap::new();
        for (local, value) in &f.local_binds {
            binds.entry(*value).or_default().push(local.0);
        }
        Self { blocks, binds }
    }
}

/// What [`prepare_debug`] made.
pub struct DebugProgram {
    pub program: InterpProgram,
    /// Each op's source location, by op number.
    pub locs: Vec<DebugLoc>,
    /// Each body's first op, by name.
    pub entries: Vec<(String, u32)>,
}

/// [`crate::prepare`] for a debugger: ops numbered, locals bound.
pub fn prepare_debug(bodies: Vec<NativeBody>, body_ids: &HashMap<String, u32>) -> DebugProgram {
    let mut locs = Vec::new();
    let mut entries = Vec::with_capacity(bodies.len());
    let mut ops = Vec::with_capacity(bodies.len());
    for b in &bodies {
        entries.push((b.name.clone(), locs.len() as u32));
        ops.push(BodyOps::number(&b.mir, &mut locs));
    }
    DebugProgram { program: crate::prepare_with(bodies, body_ids, Some(ops)), locs, entries }
}

/// The locals of one frame: local to the value last bound to it.
pub(crate) struct FrameLocals {
    body: *const Body,
    locals: HashMap<u32, u32>,
}

impl FrameLocals {
    fn new(body: *const Body) -> Self {
        // SAFETY: a frame's body lives as long as the program.
        let f = unsafe { &(*body).mir };
        Self { body, locals: f.params.iter().enumerate().map(|(i, p)| (i as u32, p.0)).collect() }
    }
}

thread_local! {
    /// The run on this thread: none but a debugged program's main thread.
    static RUN: RefCell<Option<Arc<Run>>> = const { RefCell::new(None) };
}

/// Whether this thread runs under a debugger.
pub(crate) fn active() -> bool {
    RUN.with(|r| r.borrow().is_some())
}

fn with_run<R>(f: impl FnOnce(&Arc<Run>) -> R) -> Option<R> {
    RUN.with(|r| r.borrow().as_ref().map(f))
}

enum Phase {
    Running,
    Stopped(StopReason),
    /// The thread ended; whether the program panicked.
    Done(bool),
}

/// A stop's view of the tasks.
#[derive(Default)]
struct Tasks {
    current: u64,
    live: Vec<(u64, String, String)>,
}

/// One run of the program.
struct Run {
    shared: Arc<Shared>,
    phase: Mutex<Phase>,
    wake: Condvar,
    /// Each task's activations, innermost last.
    stacks: Mutex<HashMap<u64, Vec<*mut Activation>>>,
    tasks: Mutex<Tasks>,
    /// A live context of the run, for its heap, at a stop.
    ctx: Mutex<Option<usize>>,
    panic: Mutex<Option<String>>,
}

// SAFETY: the raw pointers are only followed at a stop, when the thread that
// owns what they name waits for the debugger (`Run::stop`, `Debugger::wait`).
unsafe impl Send for Run {}
unsafe impl Sync for Run {}

impl Run {
    /// Frames of the task running, for step depth.
    fn depth(&self, task: u64) -> usize {
        let stacks = self.stacks.lock().expect("debug stacks");
        // SAFETY: the running thread's own activations.
        stacks.get(&task).map_or(0, |s| s.iter().map(|&a| unsafe { (&(*a).frames).len() }).sum())
    }

    /// Publish a stop and wait for the debugger to go on.
    fn stop(&self, reason: StopReason, ctx: *mut NativeCtx) {
        // SAFETY: the call's live context.
        let (current, live) = unsafe { rt::debug_tasks(ctx) };
        *self.tasks.lock().expect("debug tasks") = Tasks { current, live };
        *self.ctx.lock().expect("debug ctx") = Some(ctx as usize);
        let mut phase = self.phase.lock().expect("debug phase");
        *phase = Phase::Stopped(reason);
        self.wake.notify_all();
        while matches!(*phase, Phase::Stopped(_)) {
            phase = self.wake.wait(phase).expect("debug phase");
        }
    }
}

/// What the debugger and every run share.
struct Shared {
    controller: Mutex<DebugController>,
    /// `(file, line)` of each op.
    lines: Vec<Option<(u32, u32)>>,
}

/// An activation starts: it joins the running task.
///
/// # Safety
/// `act` lives until [`leave`].
pub(crate) unsafe fn enter(act: *mut Activation) {
    with_run(|run| {
        // SAFETY: as this function's.
        let (task, _) = unsafe { rt::debug_tasks((*act).ctx) };
        run.stacks.lock().expect("debug stacks").entry(task).or_default().push(act);
    });
}

/// An activation ended, or its generator yielded.
///
/// # Safety
/// `act` went through [`enter`].
pub(crate) unsafe fn leave(act: *mut Activation) {
    with_run(|run| {
        let mut stacks = run.stacks.lock().expect("debug stacks");
        for stack in stacks.values_mut() {
            if let Some(at) = stack.iter().rposition(|&a| a == act) {
                stack.remove(at);
                return;
            }
        }
    });
}

/// A new frame runs: its locals start with its parameters.
///
/// # Safety
/// `act` is the running activation.
pub(crate) unsafe fn frame_entered(act: *mut Activation) {
    // SAFETY: as this function's.
    unsafe {
        let top = (&(*act).frames).len() - 1;
        let body = (&(*act).frames)[top].body;
        let locals = &mut (*act).locals;
        locals.truncate(top);
        sync_frames(act);
        (&mut (*act).locals)[top] = FrameLocals::new(body);
    }
}

/// One [`FrameLocals`] per frame, whatever the calls did since.
unsafe fn sync_frames(act: *mut Activation) {
    // SAFETY: the running activation.
    unsafe {
        let n = (&(*act).frames).len();
        (&mut (*act).locals).truncate(n);
        while (&(*act).locals).len() < n {
            let body = (&(*act).frames)[(&(*act).locals).len()].body;
            (&mut (*act).locals).push(FrameLocals::new(body));
        }
    }
}

/// What the op before `ip` of `block` bound, in the top frame.
unsafe fn note_binds(act: *mut Activation, block: usize, ip: usize) {
    // SAFETY: the running activation.
    unsafe {
        sync_frames(act);
        let top = (&(*act).frames).len() - 1;
        let body = &*(&(*act).frames)[top].body;
        let Some(ops) = &body.ops else { return };
        let locals = &mut (&mut (*act).locals)[top];
        if !std::ptr::eq(locals.body, body) {
            *locals = FrameLocals::new(body);
        }
        let insts = &body.mir.blocks[block].insts;
        let first = body.exec().blocks[block].first as usize;
        let done = if ip == first { &insts[..first] } else { &insts[ip - 1..ip] };
        for inst in done {
            let dest = inst.dest();
            for local in ops.binds.get(&dest).into_iter().flatten() {
                locals.locals.insert(*local, dest.0);
            }
        }
    }
}

/// Before op `ip` of `block` of the top frame: stop if the debugger wants to.
///
/// # Safety
/// `act` is the running activation.
pub(crate) unsafe fn before(act: *mut Activation, block: usize, ip: usize) {
    // SAFETY: as this function's.
    unsafe {
        note_binds(act, block, ip);
        let top = (&(*act).frames).len() - 1;
        let body = &*(&(*act).frames)[top].body;
        let Some(ops) = &body.ops else { return };
        let op = ops.blocks[block] as usize + ip;
        let ctx = (*act).ctx;
        with_run(|run| {
            let (task, _) = rt::debug_tasks(ctx);
            let depth = run.depth(task);
            let stop = run.shared.controller.lock().expect("debug controller").check_stop(op, depth, run.shared.lines[op]);
            if let Some(reason) = stop {
                run.stop(reason, ctx);
            }
        });
    }
}

/// A panic is about to unwind the activation: stop with its frames. The
/// program goes no further: only a new run follows.
///
/// # Safety
/// `act` is the running activation.
pub(crate) unsafe fn panicking(act: *mut Activation, code: u64) {
    if matches!(code, TRAP_PANICKED | TRAP_CLOSE | TRAP_PARKED) {
        return;
    }
    // SAFETY: as this function's.
    unsafe {
        let Some(frame) = (&(*act).frames).last() else { return };
        if frame.ip != AT_BLOCK_START {
            note_binds(act, frame.block as usize, frame.ip as usize);
        }
        let ctx = (*act).ctx;
        with_run(|run| {
            *run.panic.lock().expect("debug panic") = Some(rt::trap_text(ctx, code));
            run.stop(StopReason::Panic, ctx);
        });
    }
}

/// The debugger's side.
pub struct Debugger {
    shared: Arc<Shared>,
    run: Option<Arc<Run>>,
}

/// One frame of a stop.
struct FrameRef {
    act: *mut Activation,
    index: usize,
}

impl Debugger {
    /// `lines` holds each op's `(file, line)`, by op number.
    pub fn new(lines: Vec<Option<(u32, u32)>>) -> Self {
        Self { shared: Arc::new(Shared { controller: Mutex::new(DebugController::new()), lines }), run: None }
    }

    /// The breakpoints and step mode, between stops.
    pub fn with_controller<R>(&self, f: impl FnOnce(&mut DebugController) -> R) -> R {
        f(&mut self.shared.controller.lock().expect("debug controller"))
    }

    /// Start `program` (it returns whether it panicked) on a thread of its
    /// own, until its first stop. A run still stopped is left there: it
    /// cannot be unwound with its frames as they are, and the process ends
    /// with the session.
    pub fn start(&mut self, program: impl FnOnce() -> bool + Send + 'static) -> StopReason {
        let run = Arc::new(Run {
            shared: Arc::clone(&self.shared),
            phase: Mutex::new(Phase::Running),
            wake: Condvar::new(),
            stacks: Mutex::new(HashMap::new()),
            tasks: Mutex::new(Tasks::default()),
            ctx: Mutex::new(None),
            panic: Mutex::new(None),
        });
        self.run = Some(Arc::clone(&run));
        let thread_run = Arc::clone(&run);
        std::thread::Builder::new()
            .name("coil-debuggee".to_string())
            .stack_size(64 << 20)
            .spawn(move || {
                RUN.with(|r| *r.borrow_mut() = Some(Arc::clone(&thread_run)));
                let panicked = program();
                *thread_run.phase.lock().expect("debug phase") = Phase::Done(panicked);
                thread_run.wake.notify_all();
            })
            .expect("spawn the debugged program's thread");
        self.wait()
    }

    /// Go on from a stop, until the next one.
    pub fn resume(&mut self) -> StopReason {
        if let Some(run) = &self.run {
            let mut phase = run.phase.lock().expect("debug phase");
            if matches!(*phase, Phase::Stopped(_)) {
                *phase = Phase::Running;
                run.wake.notify_all();
            }
        }
        self.wait()
    }

    fn wait(&self) -> StopReason {
        let Some(run) = &self.run else { return StopReason::Halt };
        let mut phase = run.phase.lock().expect("debug phase");
        loop {
            match &*phase {
                Phase::Running => phase = run.wake.wait(phase).expect("debug phase"),
                Phase::Stopped(reason) => return reason.clone(),
                Phase::Done(true) => return StopReason::Panic,
                Phase::Done(false) => return StopReason::Halt,
            }
        }
    }

    /// The message of the panic the run stopped at.
    pub fn panic_message(&self) -> Option<String> {
        self.run.as_ref()?.panic.lock().expect("debug panic").clone()
    }

    fn stopped(&self) -> Option<&Arc<Run>> {
        let run = self.run.as_ref()?;
        matches!(*run.phase.lock().expect("debug phase"), Phase::Stopped(_)).then_some(run)
    }

    /// The running task's frames at a stop, outermost first.
    fn frames(&self) -> Vec<FrameRef> {
        let Some(run) = self.stopped() else { return Vec::new() };
        let task = run.tasks.lock().expect("debug tasks").current;
        Self::task_frames(run, task)
    }

    fn task_frames(run: &Run, task: u64) -> Vec<FrameRef> {
        let stacks = run.stacks.lock().expect("debug stacks");
        let mut out = Vec::new();
        for &act in stacks.get(&task).into_iter().flatten() {
            // SAFETY: at a stop the activations stand still.
            let n = unsafe { (&(*act).frames).len() };
            out.extend((0..n).map(|index| FrameRef { act, index }));
        }
        out
    }

    /// Frames at a stop.
    pub fn depth(&self) -> usize {
        self.frames().len()
    }

    fn op_of(frame: &FrameRef) -> Option<usize> {
        // SAFETY: at a stop the activations stand still; a frame's body
        // lives as long as the program.
        unsafe {
            let f = &(&(*frame.act).frames)[frame.index];
            let body = &*f.body;
            let ops = body.ops.as_ref()?;
            let ip = if f.ip == AT_BLOCK_START { body.exec().blocks[f.block as usize].first } else { f.ip };
            Some(ops.blocks[f.block as usize] as usize + ip as usize)
        }
    }

    /// The op frame `i` stands at (below the top frame, its call).
    pub fn frame_op(&self, i: usize) -> Option<usize> {
        Self::op_of(self.frames().get(i)?)
    }

    /// Frame `i`'s first register.
    pub fn frame_base(&self, i: usize) -> Option<usize> {
        let frames = self.frames();
        let f = frames.get(i)?;
        // SAFETY: at a stop the activations stand still.
        Some(unsafe { (&(*f.act).frames)[f.index].base })
    }

    /// The word of local `local` in frame `i`, once something bound it.
    pub fn local(&self, i: usize, local: u32) -> Option<Value> {
        let frames = self.frames();
        let f = frames.get(i)?;
        // SAFETY: at a stop the activations stand still.
        unsafe {
            let act = &*f.act;
            let value = *act.locals.get(f.index)?.locals.get(&local)?;
            let base = act.frames[f.index].base;
            Some(Value::from(act.regs[base + reg(ValueId(value))]))
        }
    }

    fn heap(&self) -> Option<&machine::Heap> {
        let run = self.stopped()?;
        let ctx = (*run.ctx.lock().expect("debug ctx"))? as *mut NativeCtx;
        // SAFETY: the context of the call stopped here; its heap stands still.
        unsafe { (*ctx).heap.as_ref() }
    }

    /// The heap object at `v`, at a stop.
    pub fn object(&self, v: Value) -> Option<DebugObject> {
        machine::debug::object_view(self.heap()?, v)
    }

    /// `v` as `print` shows a word of no known type, at a stop.
    pub fn format(&self, v: Value) -> String {
        self.heap().map_or_else(|| format!("{:#x}", v.bits()), |h| machine::debug::format_word(h, v))
    }

    /// The tasks at a stop; none without child tasks. The running one's
    /// frames are live, the others' are their ops.
    pub fn tasks(&self) -> Vec<DebugTask> {
        let Some(run) = self.stopped() else { return Vec::new() };
        let tasks = run.tasks.lock().expect("debug tasks");
        if tasks.live.len() < 2 {
            return Vec::new();
        }
        tasks
            .live
            .iter()
            .map(|(id, name, state)| {
                let frames = Self::task_frames(run, *id);
                let frames = if *id == tasks.current {
                    DebugTaskFrames::Live(0..frames.len())
                } else {
                    DebugTaskFrames::Saved(frames.iter().filter_map(Self::op_of).collect())
                };
                DebugTask { id: *id, name: name.clone(), state: state.clone(), frames }
            })
            .collect()
    }
}
