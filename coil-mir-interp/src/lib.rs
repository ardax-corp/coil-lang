//! An interpreter of MIR: the engine for what has no machine code to run
//! (the playground, the debugger, compile-time code). See
//! `docs/internals/mir-interp.md`.
//!
//! At `-O 0` it runs MIR as lowered, with no MIR pass on it
//! (`compiler::native_mir::start_raw_capture`): the reference output every
//! other level and engine is compared against. At any other level it runs
//! the MIR native code compiles, so a difference from the reference is a
//! pass's and a difference from native code at the same level the backend's.
//!
//! It is one more backend of the runtime's native ABI: [`prepare`] turns a
//! program's captured MIR into [`NativeEntry`]s, as `coil_native::jit`
//! does, and each op does what Cranelift's lowering of it does, through
//! the same `coil_native_*` helpers. The driver, the collector, tasks and
//! generators see no difference.
//!
//! What differs from machine code is the frames. They live in an
//! [`Activation`], not on the thread's stack: a direct call pushes a frame
//! and the loop goes on, so recursion costs no Rust stack, and the words a
//! frame holds stay where they are. The collector reaches them through one
//! [`ROOTS_SCAN`] record per activation, so a call pays nothing for roots,
//! and the liveness that says which registers count is computed only for
//! the bodies a collection finds on the stack.

// Activations are reached through raw pointers (the collector walks them while
// the loop runs). A method call through one must spell its borrow out, or
// rustc's `dangerous_implicit_autorefs` fires; clippy then calls it needless.
#![allow(clippy::needless_borrow)]

pub mod debug;
mod exec;
mod frame;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use common::DebugLoc;
use compiler::native_mir::{BlockId, Label, MirFunc, MirInst, NativeBody, Terminator, ValueId, is_generator, is_tail_call_inst, used_params};
use machine::native::{NativeEntry, NativeSite};

use frame::RootMaps;

/// What [`prepare`] produced.
pub struct InterpProgram {
    /// Entries to install, as `coil_native::NativeProgram`'s.
    pub entries: Vec<NativeEntry>,
    /// Bodies the interpreter runs.
    pub compiled: Vec<String>,
    /// Bodies it cannot run, with the reason.
    pub refused: Vec<(String, String)>,
    /// The panic sites the bodies name.
    pub sites: Vec<NativeSite>,
    /// What `entries` point into: keep it for as long as they run
    /// (`machine::native::NativeCode::with_keep`).
    pub keep: Arc<Program>,
}

/// The bodies of a program.
pub struct Program {
    bodies: Vec<Body>,
    /// By [`Label`], `u32::MAX` for a label with no body.
    by_label: Vec<u32>,
    /// By body id: the bodies a function value may run as a frame.
    by_pc: HashMap<usize, u32>,
}

impl Program {
    fn body_at_pc(&self, pc: usize) -> Option<&Body> {
        self.bodies.get(*self.by_pc.get(&pc)? as usize)
    }

    fn body_at(&self, target: Label) -> Option<&Body> {
        let i = *self.by_label.get(target.0 as usize)?;
        self.bodies.get(i as usize)
    }
}

/// One body, as it runs.
pub(crate) struct Body {
    pub(crate) mir: MirFunc,
    pub(crate) program: *const Program,
    /// Entry id (`u32::MAX`: none).
    pub(crate) pc: u32,
    /// The leading parameters it reads ([`used_params`]): a call of fewer
    /// words is refused, as the native driver refuses one.
    pub(crate) reads: u32,
    /// Its cleanup pad, run as a body of its own.
    pub(crate) pad: Option<Box<Body>>,
    /// Why it cannot run: a call goes through the host.
    pub(crate) refused: bool,
    /// A `drop`: a call claims the finalizer first.
    pub(crate) is_drop: bool,
    /// `sites[block][op]`: the op's site; one past the last is the
    /// terminator's.
    sites: Vec<Vec<u32>>,
    /// The site of a frame with no op running (a stack overflow on entry).
    pub(crate) entry_site: u32,
    /// Filled by the body's first call.
    exec: OnceLock<Exec>,
    /// Filled by the first collection that finds the body on the stack.
    pub(crate) roots: OnceLock<RootMaps>,
    /// Under a debugger: its op numbers and locals.
    pub(crate) ops: Option<debug::BodyOps>,
}

// SAFETY: `program` points at the `Program` that owns this body, which is
// immutable once built and outlives every call; all else is `Sync`.
unsafe impl Sync for Body {}
unsafe impl Send for Body {}

/// A block's control flow, resolved to block positions.
pub(crate) struct ExecBlock {
    /// The first op past the φs.
    pub(crate) first: u32,
    /// A loop header: a safepoint.
    pub(crate) header: bool,
    /// It ends in a tail call: its last op takes the frame.
    pub(crate) tail: bool,
    /// Where each successor leads (`taken` first), with its φ moves.
    pub(crate) edges: [Option<Edge>; 2],
}

/// A CFG edge: the block it reaches and the φ moves it makes.
pub(crate) struct Edge {
    pub(crate) to: u32,
    pub(crate) moves: Vec<(u32, u32)>,
    /// It goes back in block order: a step.
    pub(crate) back: bool,
}

pub(crate) struct Exec {
    pub(crate) blocks: Vec<ExecBlock>,
    pub(crate) entry: u32,
}

impl Body {
    pub(crate) fn exec(&self) -> &Exec {
        self.exec.get_or_init(|| Exec::new(&self.mir))
    }

    /// The site of op `ip` of `block` (the terminator's past the last).
    pub(crate) fn site_at(&self, block: u32, ip: u32) -> u32 {
        let ops = &self.sites[block as usize];
        ops.get(ip as usize).copied().unwrap_or(self.entry_site)
    }
}

impl Exec {
    fn new(f: &MirFunc) -> Self {
        let pos: HashMap<BlockId, u32> = f.blocks.iter().enumerate().map(|(i, b)| (b.id, i as u32)).collect();
        let edge = |from: BlockId, to: BlockId| -> Option<Edge> {
            let at = *pos.get(&to)?;
            let moves = f.blocks[at as usize]
                .insts
                .iter()
                .filter_map(|i| match i {
                    MirInst::Phi { dest, args, .. } => {
                        let (_, v) = args.iter().find(|(p, _)| *p == from)?;
                        Some((dest.0, v.0))
                    }
                    _ => None,
                })
                .collect();
            Some(Edge { to: at, moves, back: false })
        };
        let mut blocks: Vec<ExecBlock> = f
            .blocks
            .iter()
            .map(|b| {
                let edges = match &b.term {
                    Some(Terminator::Jump { dest }) => [edge(b.id, *dest), None],
                    Some(Terminator::Br { taken, not_taken, .. } | Terminator::JumpIfMatch { taken, not_taken, .. }) => {
                        [edge(b.id, *taken), edge(b.id, *not_taken)]
                    }
                    _ => [None, None],
                };
                let first = b.insts.iter().take_while(|i| i.is_phi()).count() as u32;
                let tail = b.insts.last().is_some_and(|i| is_tail_call_inst(f, b, i));
                ExecBlock { first, header: false, tail, edges }
            })
            .collect();
        // Loop headers: the targets of edges back in block order.
        for i in 0..blocks.len() {
            for e in 0..2 {
                if let Some(edge) = &mut blocks[i].edges[e]
                    && edge.to as usize <= i
                {
                    edge.back = true;
                    let to = edge.to as usize;
                    blocks[to].header = true;
                }
            }
        }
        Self { blocks, entry: pos.get(&f.entry).copied().unwrap_or(0) }
    }
}

/// Why a body cannot run here.
fn refusal(f: &MirFunc) -> Option<String> {
    if let Some(why) = f.emit_refusal {
        return Some(format!("no emission: {why}"));
    }
    let insts = f.blocks.iter().flat_map(|b| &b.insts);
    for inst in insts {
        match inst {
            MirInst::ByteScan { .. } | MirInst::VecLoop { .. } if compiler::native_mir::vector_op(inst).is_none() => {
                return Some("a vector op the runtime's encoding cannot hold".to_string());
            }
            MirInst::Alloc { kind: compiler::native_mir::MirAllocKind::Object { .. }, elems, .. } if !elems.is_empty() => {
                return Some("an object allocated with its fields".to_string());
            }
            _ => {}
        }
    }
    if f.blocks.iter().any(|b| b.term.is_none()) {
        return Some("block without terminator".to_string());
    }
    None
}

/// Each op's site in `f`, adding to `sites`: what `coil_native`'s
/// lowering records, shared by the ops of one location.
fn body_sites(f: &MirFunc, pc: u32, sites: &mut Vec<NativeSite>) -> (Vec<Vec<u32>>, u32) {
    // Keyed by the whole location: two splices of one callee share its
    // spans but not their inline scopes, which name the call (#2868).
    let mut seen: HashMap<(u32, u32, u32, u32), u32> = HashMap::new();
    let mut site = |loc: DebugLoc, sites: &mut Vec<NativeSite>| -> u32 {
        *seen.entry((loc.file, loc.start_byte, loc.end_byte, loc.scope)).or_insert_with(|| {
            sites.push(NativeSite { body_id: pc, loc });
            (sites.len() - 1) as u32
        })
    };
    let entry = site(DebugLoc::unknown(), sites);
    let ops = f
        .blocks
        .iter()
        .map(|b| {
            let mut ops: Vec<u32> = b.insts.iter().map(|i| site(f.op_loc(i.dest()), sites)).collect();
            ops.push(entry);
            ops
        })
        .collect();
    (ops, entry)
}

fn build_body(mir: MirFunc, program: *const Program, pc: Option<u32>, sites: &mut Vec<NativeSite>) -> Body {
    let (mut mir, cleanup) = {
        let mut m = mir;
        let c = m.cleanup.take();
        (m, c)
    };
    let refused = refusal(&mir).is_some();
    let code = pc.unwrap_or(u32::MAX);
    let pad = cleanup.map(|c| Box::new(build_body(*c, program, pc, sites)));
    let is_drop = mir.name.ends_with("::drop") && mir.params.len() == 1;
    let reads = used_params(&mir) as u32;
    let (op_sites, entry_site) = if pc.is_some() { body_sites(&mir, code, sites) } else { (mir.blocks.iter().map(|b| vec![machine::native::NO_SITE as u32; b.insts.len() + 1]).collect(), machine::native::NO_SITE as u32) };
    mir.shrink_to_fit();
    Body { mir, program, pc: code, reads, pad, refused, is_drop, sites: op_sites, entry_site, exec: OnceLock::new(), roots: OnceLock::new(), ops: None }
}

/// `COIL_MIR_ARCHIVE_CHECK`: what comes back from the archive format, which
/// must be what went in. Running that copy tests the format on a whole
/// corpus.
fn through_archive(bodies: Vec<NativeBody>, body_ids: &HashMap<String, u32>) -> Vec<NativeBody> {
    let bytes = compiler::native_mir::archive::encode(&bodies, body_ids, &[]);
    let (back, pcs) = compiler::native_mir::archive::decode(&bytes).expect("the archive format reads what it wrote");
    assert_eq!(back.len(), bodies.len(), "bodies through the archive format");
    for (a, b) in bodies.iter().zip(&back) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.entry, b.entry, "{}", a.name);
        assert!(a.mir == b.mir, "{} changed through the archive format", a.name);
        assert_eq!(body_ids.get(&a.name), pcs.get(&a.name), "{}", a.name);
    }
    back
}

/// Make `bodies` runnable. `body_ids` maps a body's name to its
/// body id, as for `coil_native::jit`.
pub fn prepare(bodies: Vec<NativeBody>, body_ids: &HashMap<String, u32>) -> InterpProgram {
    prepare_with(bodies, body_ids, None)
}

/// [`prepare`], each body with its [`debug::BodyOps`] when a debugger runs it.
pub(crate) fn prepare_with(bodies: Vec<NativeBody>, body_ids: &HashMap<String, u32>, ops: Option<Vec<debug::BodyOps>>) -> InterpProgram {
    let mut ops = ops.map(Vec::into_iter);
    let bodies = if std::env::var_os("COIL_MIR_ARCHIVE_CHECK").is_some() { through_archive(bodies, body_ids) } else { bodies };
    let mut sites = Vec::new();
    let mut compiled = Vec::new();
    let mut refused = Vec::new();
    let labels = bodies.iter().filter_map(|b| b.entry).map(|l| l.0 as usize + 1).max().unwrap_or(0);
    let mut by_label = vec![u32::MAX; labels];
    let mut program = Arc::new(Program { bodies: Vec::new(), by_label: Vec::new(), by_pc: HashMap::new() });
    let at = Arc::as_ptr(&program);
    let mut built = Vec::with_capacity(bodies.len());
    for (i, b) in bodies.into_iter().enumerate() {
        if let Some(l) = b.entry {
            by_label[l.0 as usize] = i as u32;
        }
        let why = refusal(&b.mir);
        match why {
            Some(why) => refused.push((b.name.clone(), why)),
            None => compiled.push(b.name.clone()),
        }
        let pc = body_ids.get(&b.name).copied();
        let mut body = build_body(b.mir, at, pc, &mut sites);
        body.ops = ops.as_mut().and_then(Iterator::next);
        built.push(body);
    }
    let p = Arc::get_mut(&mut program).expect("a program nothing else holds yet");
    p.by_pc = built.iter().enumerate().filter(|(_, b)| b.pc != u32::MAX && !b.refused && !is_generator(&b.mir)).map(|(i, b)| (b.pc as usize, i as u32)).collect();
    p.bodies = built;
    p.by_label = by_label;
    let entries = program
        .bodies
        .iter()
        .filter(|b| b.pc != u32::MAX && !b.refused)
        .map(|b| NativeEntry {
            pc: b.pc,
            arity: b.mir.params.len() as u16,
            // Any call may reach a host call that waits.
            suspends: true,
            reads: b.reads as u16,
            ret_words: if b.mir.ret_hi_ty.is_some() { 2 } else { 1 },
            generator: is_generator(&b.mir),
            cleanup: b.pad.is_some(),
            entry: exec::entry,
            data: std::ptr::from_ref(b) as usize,
            twin: 0,
        })
        .collect();
    InterpProgram { entries, compiled, refused, sites, keep: program }
}

/// `value`'s position in a frame's registers.
#[inline(always)]
pub(crate) fn reg(v: ValueId) -> usize {
    v.index()
}

#[cfg(test)]
mod tests {
    use super::*;
    use compiler::native_mir::{MirBlock, MirTy};
    use machine::native::{NATIVE_STACK_BUDGET, NativeCtx};

    /// `f(v0, v1)`: two `rc.inc v0`, then `rc.dec v0 if v1`; returns `v1`.
    /// `g(v0)`: `rc.dec v0`.
    fn counting_bodies() -> Vec<NativeBody> {
        let body = |name: &str, label: u32, ops: Vec<MirInst>, params: u32| {
            let mut f = MirFunc::new(name);
            f.blocks.clear();
            f.types = vec![MirTy::HeapRef];
            f.types.resize(params as usize + ops.len(), MirTy::Bool);
            f.params = (0..params).map(ValueId).collect();
            f.ret_ty = Some(MirTy::Bool);
            let mut b = MirBlock::new(BlockId(0));
            b.insts = ops;
            b.term = Some(Terminator::Return { lo: Some(ValueId(params - 1)), hi: None });
            f.blocks.push(b);
            NativeBody { name: name.into(), entry: Some(Label(label)), mir: f }
        };
        let f = vec![
            MirInst::RcInc { dest: ValueId(2), src: ValueId(0), guard: None },
            MirInst::RcInc { dest: ValueId(3), src: ValueId(0), guard: None },
            MirInst::RcDec { dest: ValueId(4), src: ValueId(0), guard: Some(ValueId(1)) },
        ];
        let g = vec![MirInst::RcDec { dest: ValueId(1), src: ValueId(0), guard: None }];
        vec![body("f", 0, f, 2), body("g", 1, g, 1)]
    }

    /// The reference count ops (plan step 3), as `coil_native`'s test of
    /// the same bodies: an increment, a guarded decrement that applies only
    /// when its guard is true, and a decrement of the last owner that
    /// frees the object.
    #[test]
    fn the_count_ops_move_the_count_and_the_last_one_frees() {
        let mut heap = machine::Heap::default();
        heap.set_rc_counting(true);
        heap.set_rc_frees(true);
        let (obj, _) = heap.alloc(machine::ObjString::from("counted"), machine::Object::String);
        obj.rc_inc();
        let pcs = HashMap::from([("f".to_string(), 0u32), ("g".to_string(), 1u32)]);
        let p = prepare(counting_bodies(), &pcs);
        assert!(p.refused.is_empty(), "refused: {:?}", p.refused);
        let call = |heap: &mut machine::Heap, pc: u32, args: &[u64]| {
            let e = p.entries.iter().find(|e| e.pc == pc).expect("entry");
            let marker = 0u8;
            let mut ctx = NativeCtx::new((std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET));
            ctx.heap = heap;
            ctx.entry_data = e.data as u64;
            let mut fuel = u64::MAX;
            ctx.fuel = &mut fuel;
            // SAFETY: `e` runs a body of `args.len()` parameters.
            unsafe { (e.entry)(&mut ctx, args.as_ptr()) };
            assert_eq!(ctx.bail, 0);
        };
        call(&mut heap, 0, &[obj.addr(), 0]);
        assert_eq!(obj.rc_count(), 3, "the guarded decrement did not run");
        call(&mut heap, 0, &[obj.addr(), 1]);
        assert_eq!(obj.rc_count(), 4);
        let live = heap.live_object_count();
        for _ in 0..3 {
            call(&mut heap, 1, &[obj.addr()]);
        }
        assert_eq!((obj.rc_count(), heap.live_object_count()), (1, live));
        call(&mut heap, 1, &[obj.addr()]);
        assert_eq!(heap.live_object_count(), live - 1, "the last owner's decrement frees it");
    }
}
