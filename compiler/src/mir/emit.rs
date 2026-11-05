//! Lower verified numeric MIR to dense bytecode (`IlOp` residuals + labels).

use std::collections::{HashMap, HashSet};

use common::{dense, Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::call_convoy::{is_tail_call_inst, ConvoyPlan};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp,
    Terminator, ValueId,
};
use super::ty::MirTy;

/// The panic write-backs of a body with [`MirInst::PanicStore`]s: a panic
/// at a pc from `start` up to `end` (the next label) continues at `pad`,
/// which does the stores the source owed the heap there and resumes the
/// unwind. `frame` is the body's frame words.
#[derive(Clone, Debug, Default)]
pub struct PanicPads {
    pub ranges: Vec<(Label, Label, Label)>,
    pub frame: u32,
}

/// The body dense emits and its registers, as emit and its sidecars see them.
struct DenseLayout {
    func: MirFunc,
    plan: ConvoyPlan,
    regs: Vec<u8>,
    scratch: u8,
    /// Registers are shared by liveness (#2242).
    shared: bool,
}

#[cfg(test)]
thread_local! {
    /// Blocks scanned answering [`RewriteScan`] queries, for the work
    /// bound of #1493.
    pub(super) static REWRITE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether a collection can run, from a point of a block, while a register
/// still holds what an edge left in it: on some path an op that may collect
/// (or, in a body with a cleanup pad, trap) comes before an op that rewrites
/// the register or the frame's exit. Otherwise a zero of the register there
/// is dead (a hot loop's join that allocates nothing).
///
/// Answered from the top of every block at once, per register asked about:
/// one pass over the body and its edges, not a walk of every block a query
/// reaches. A chain of joins asked once per copy source each took ~n³
/// block scans (#1493).
struct RewriteScan<'f> {
    func: &'f MirFunc,
    /// Per block, the ops (past its φs) that may collect, by index.
    loud: Vec<Vec<usize>>,
    /// Per block, the ops (past its φs) whose dest has a register, by index,
    /// and whether the op writes it before it runs (a push's gather
    /// register, [`Gathers`]) rather than as its result.
    writes: Vec<Vec<(usize, u8, bool)>>,
    preds: Vec<Vec<usize>>,
    /// Per register asked, whether a collection may come first from the
    /// top of each block.
    from_top: HashMap<u8, Vec<bool>>,
}

impl<'f> RewriteScan<'f> {

}

#[cfg(test)]
thread_local! {
    /// Values [`CopySources`] stepped through, for the work bound of #1502.
    pub(super) static COPY_SOURCE_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Where a value's word was copied from: a cast's source (an
/// `Option::Some` wrap) and a φ's incoming values, transitively. Each keeps
/// the word in its own register after its last use, so once the value is
/// rebound they still root it until rewritten (#1411).
pub(super) struct CopySources<'f> {
    /// Casts and φs, with the block that defines them.
    defs: HashMap<ValueId, (BlockId, &'f MirInst)>,
    /// Constants and casts of them (a `None` niche): they root nothing.
    consts: HashSet<ValueId>,
}

impl<'f> CopySources<'f> {

}

impl CopySources<'_> {

}

/// A set of frame registers.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RegSet([u64; 4]);

impl RegSet {

}

/// Per value, the registers of all its copy sources ([`CopySources`]) that
/// have a slot, along every edge and past every value: what any walk of
/// its sources can meet, at most. And, where the sources form no cycle,
/// the registers [`CopySources::new_regs_along`] meets first, in its order,
/// so a walk past no live source and along every edge need not run: a
/// chain of joins holding a register only at its start (the first `s`)
/// was walked to that start from every join (#1502).
pub(super) struct SourceRegs {
    below: Vec<RegSet>,
    /// Where each value is defined (block, then op): a source defined
    /// before its user all the way down forms no cycle.
    pos: Vec<(u32, u32)>,
    /// Whether a value is some copy's source: only such a value, live,
    /// stops a walk.
    is_source: Vec<bool>,
    /// The registers a walk from a value meets first, in walk order
    /// (sources past params only); `None` past a cycle.
    first: Vec<Option<Vec<u8>>>,
}

impl SourceRegs {

}

/// The heap words a value's word was copied to or holds, transitively:
/// its casts (a match arm's binding of an `Option::Some`'s object), its
/// match payloads (an arm's binding of a boxed enum's field) and, for an
/// enum or tuple it makes, the elements it was made from. The reverse of
/// [`CopySources`] through casts: a φ's copy is cleared past its edge
/// ([`entry_clears`]). Each keeps its word in its own register after its
/// last use, a call's argument or a constructor's element, so once the
/// value is rebound they still root it until rewritten (#1625).
pub(super) struct CopySinks {
    casts: HashMap<ValueId, Vec<ValueId>>,
}

impl CopySinks {

}

/// The gather registers a dense body's pushes copy heap words into
/// ([`gather_base`]: arguments not in consecutive registers are moved to
/// the scratch window first). Nothing else writes those registers, so
/// each keeps the last word copied there after the call: the argument's
/// object stays rooted however its value is cleared (#1625). The LIR tier
/// pushes from the value's own register and gathers nothing.
#[derive(Default)]
pub(super) struct Gathers {
    /// Per argument value, the scratch registers it may be copied into.
    by_value: HashMap<ValueId, Vec<u8>>,
    /// The scratch registers each push writes, by block index and op
    /// index: before the op runs.
    pub(super) writes: Vec<(usize, usize, u8)>,
    /// Per push (block index, op index), the scratch registers it gathers
    /// words into and the word each takes.
    by_push: HashMap<(usize, usize), Vec<(u8, ValueId)>>,
}

impl Gathers {

}

/// The zeros [`arg_clears`] places: after a call (by its dest), or at the
/// top of a block, below its label.
#[derive(Default)]
pub(super) struct ArgClears {
    pub(super) after: HashMap<ValueId, Vec<u8>>,
    pub(super) entry: HashMap<BlockId, Vec<u8>>,
}

impl ArgClears {

}


pub(super) struct EmitBrCondArgs<'args> {
    out: &'args mut Vec<IlOp>,
    block: &'args super::func::MirBlock,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    cond: ValueId,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

pub(super) struct EmitInstArgs<'args> {
    pub(super) out: &'args mut Vec<IlOp>,
    pub(super) inst: &'args MirInst,
    pub(super) func: &'args MirFunc,
    pub(super) regs: &'args [u8],
    pub(super) scratch: u8,
    pub(super) pool: &'args mut Vec<u64>,
    pub(super) loc: DebugLoc,
    pub(super) across_alloc: bool,
}

struct EmitTermArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    block: &'args super::func::MirBlock,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    scratch: u8,
    block_lab: &'args [Label],
    next_label: &'args mut u32,
    reserved: &'args HashSet<u32>,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
    /// Registers zeroed after an edge's φ copies ([`entry_clears`]).
    clears: &'args HashMap<(BlockId, BlockId), Vec<u8>>,
    /// Loop headers a back edge copies its compare-branch into ([`rotatable_headers`]).
    rotate: &'args [bool],
}

struct EmitDenseJumpIfMatchArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    block: &'args super::func::MirBlock,
    scrutinee: ValueId,
    tag: u32,
    payloads: &'args [ValueId],
    taken: BlockId,
    not_taken: BlockId,
    regs: &'args [u8],
    scratch: u8,
    block_lab: &'args [Label],
    next_label: &'args mut u32,
    reserved: &'args HashSet<u32>,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

struct EmitBoxedLastArmUnpackArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    func: &'args MirFunc,
    block: BlockId,
    scrutinee: ValueId,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

struct EmitBoxedJumpIfMatchArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    block: &'args super::func::MirBlock,
    scrutinee: ValueId,
    tag: u32,
    payloads: &'args [ValueId],
    taken: BlockId,
    not_taken: BlockId,
    regs: &'args [u8],
    scratch: u8,
    block_lab: &'args [Label],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

struct EmitCallArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    kind: crate::il::EntryKind,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    target: crate::il::Label,
    args: &'args [ValueId],
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    scratch: u8,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

struct EmitArgsOnStackArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    args: &'args [ValueId],
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

struct EmitStackValueArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    v: ValueId,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

struct EmitStackBinArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    dest: ValueId,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

/// A `VecLoop`'s `VLoop` body: each tree node into the next free vector
/// register (freed again after the node), a reduce's `VReduce` into the
/// loop's `dest` slot `d`; slot `p` holds the block position for an index.
pub(crate) fn vec_loop_body(kind: &super::inst::VecLoopKind, regs: &[u8], d: u8, p: u8) -> Option<Vec<Byte>> {
    use super::inst::{VecExpr, VecLoopKind};
    use common::simd;

    struct Ctx<'a> {
        out: Vec<Byte>,
        next: u8,
        regs: &'a [u8],
        p: u8,
        /// [`Ctx::shared`]'s registers by node, the readers each node has
        /// left, and the registers free again.
        memo: std::collections::HashMap<VecExpr, u8>,
        uses: std::collections::HashMap<VecExpr, usize>,
        free: Vec<u8>,
    }

    impl Ctx<'_> {
        /// One store's value or the fold's term in a register: each
        /// distinct node once ([`Ctx::shared`]), or, when that needs more
        /// than the registers, as the tree ([`Ctx::eval`]).
        fn value(&mut self, e: &VecExpr) -> Option<u8> {
            let at = self.out.len();
            self.next = 0;
            self.memo.clear();
            self.uses.clear();
            self.free.clear();
            self.count(e, false);
            if let Some(r) = self.shared(e, true) {
                return Some(r);
            }
            self.out.truncate(at);
            self.next = 0;
            self.eval(e)
        }

        /// How many parents read each shared node (`owned`: the node a
        /// `SELECT` / `VFma` writes into, computed again for each).
        fn count(&mut self, e: &VecExpr, owned: bool) {
            if !owned {
                let n = self.uses.entry(e.clone()).or_insert(0);
                *n += 1;
                if *n > 1 {
                    return;
                }
            }
            match e {
                VecExpr::Splat { .. } | VecExpr::Index | VecExpr::Load { .. } => {}
                VecExpr::Bin { lhs, rhs, .. } => {
                    self.count(lhs, false);
                    self.count(rhs, false);
                }
                VecExpr::Unary { src, .. } => self.count(src, false),
                VecExpr::Select { mask, then, els } => {
                    self.count(mask, true);
                    self.count(then, false);
                    self.count(els, false);
                }
                VecExpr::Fma { a, b, c, .. } => {
                    self.count(c, true);
                    self.count(a, false);
                    self.count(b, false);
                }
            }
        }

        /// A register for [`Ctx::shared`]: a freed one first.
        fn sreg(&mut self) -> Option<u8> {
            self.free.pop().or_else(|| self.reg())
        }

        /// A parent has read shared node `e`; its register is free after
        /// its last reader.
        fn release(&mut self, e: &VecExpr) {
            if let Some(n) = self.uses.get_mut(e) {
                *n -= 1;
                if *n == 0
                    && let Some(r) = self.memo.remove(e)
                {
                    self.free.push(r);
                }
            }
        }

        /// `e` with a node seen before in this value read from its
        /// register, which is reused once its last reader ran. The node a
        /// `VBin SELECT` / `VFma` writes into is computed again for it, not
        /// shared (`memo` false).
        fn shared(&mut self, e: &VecExpr, memo: bool) -> Option<u8> {
            if memo && let Some(&r) = self.memo.get(e) {
                return Some(r);
            }
            let r = match e {
                VecExpr::Splat { v, ty } => {
                    let r = self.sreg()?;
                    let k = if *ty == dense::TY_F64 { simd::SPLAT_F64 } else { simd::SPLAT_I64 };
                    self.word(Instruction::VBin, k, r, self.slot(*v), 0);
                    r
                }
                VecExpr::Index => {
                    let (r, p) = (self.sreg()?, self.sreg()?);
                    self.word(Instruction::VBin, simd::IOTA_I64, r, 0, 0);
                    self.word(Instruction::VBin, simd::SPLAT_I64, p, self.p, 0);
                    self.word(Instruction::VBin, simd::IADD64, r, r, p);
                    self.free.push(p);
                    r
                }
                VecExpr::Load { array, off, ty } => {
                    let r = self.sreg()?;
                    self.word(Instruction::VLoad, *ty, r, self.slot(*array), self.slot(*off));
                    r
                }
                VecExpr::Bin { kind, lhs, rhs } => {
                    let x = self.shared(lhs, true)?;
                    let y = self.shared(rhs, true)?;
                    self.release(lhs);
                    self.release(rhs);
                    let r = self.sreg()?;
                    self.word(Instruction::VBin, *kind, r, x, y);
                    r
                }
                VecExpr::Unary { kind, src } => {
                    let x = self.shared(src, true)?;
                    self.release(src);
                    let r = self.sreg()?;
                    self.word(Instruction::VBin, *kind, r, x, 0);
                    r
                }
                VecExpr::Select { mask, then, els } => {
                    let m = self.shared(mask, false)?;
                    let x = self.shared(then, true)?;
                    let y = self.shared(els, true)?;
                    self.word(Instruction::VBin, simd::SELECT, m, x, y);
                    self.release(then);
                    self.release(els);
                    m
                }
                VecExpr::Fma { ty, a, b, c } => {
                    let z = self.shared(c, false)?;
                    let x = self.shared(a, true)?;
                    let y = self.shared(b, true)?;
                    self.word(Instruction::VFma, *ty, z, x, y);
                    self.release(a);
                    self.release(b);
                    z
                }
            };
            if memo {
                self.memo.insert(e.clone(), r);
            }
            Some(r)
        }

        fn reg(&mut self) -> Option<u8> {
            let r = self.next;
            if usize::from(r) >= simd::NREGS {
                return None;
            }
            self.next += 1;
            Some(r)
        }

        fn word(&mut self, op: Instruction, kind: u8, dest: u8, a: u8, b: u8) {
            self.out.push(Byte::new(op).with_dense_abc(kind, dest, a, b));
        }

        fn slot(&self, v: ValueId) -> u8 {
            self.regs[v.index()]
        }

        fn eval(&mut self, e: &VecExpr) -> Option<u8> {
            let base = self.next;
            let r = match e {
                VecExpr::Splat { v, ty } => {
                    let r = self.reg()?;
                    let k = if *ty == dense::TY_F64 { simd::SPLAT_F64 } else { simd::SPLAT_I64 };
                    self.word(Instruction::VBin, k, r, self.slot(*v), 0);
                    r
                }
                // `p + k`: the lane offsets plus the position in `d`.
                VecExpr::Index => {
                    let (r, p) = (self.reg()?, self.reg()?);
                    self.word(Instruction::VBin, simd::IOTA_I64, r, 0, 0);
                    self.word(Instruction::VBin, simd::SPLAT_I64, p, self.p, 0);
                    self.word(Instruction::VBin, simd::IADD64, r, r, p);
                    r
                }
                VecExpr::Load { array, off, ty } => {
                    let r = self.reg()?;
                    self.word(Instruction::VLoad, *ty, r, self.slot(*array), self.slot(*off));
                    r
                }
                VecExpr::Bin { kind, lhs, rhs } => {
                    let x = self.eval(lhs)?;
                    let y = self.eval(rhs)?;
                    self.word(Instruction::VBin, *kind, x, x, y);
                    x
                }
                VecExpr::Unary { kind, src } => {
                    let x = self.eval(src)?;
                    self.word(Instruction::VBin, *kind, x, x, 0);
                    x
                }
                // `VBin SELECT` picks into its dest, which holds the mask.
                VecExpr::Select { mask, then, els } => {
                    let m = self.eval(mask)?;
                    let x = self.eval(then)?;
                    let y = self.eval(els)?;
                    self.word(Instruction::VBin, simd::SELECT, m, x, y);
                    m
                }
                // `VFma` adds into its dest: `c` first, so it lands at `base`.
                VecExpr::Fma { ty, a, b, c } => {
                    let z = self.eval(c)?;
                    let x = self.eval(a)?;
                    let y = self.eval(b)?;
                    self.word(Instruction::VFma, *ty, z, x, y);
                    z
                }
            };
            debug_assert_eq!(r, base);
            self.next = base + 1;
            Some(r)
        }
    }

    let mut cx = Ctx {
        out: Vec::new(),
        next: 0,
        regs,
        p,
        memo: std::collections::HashMap::new(),
        uses: std::collections::HashMap::new(),
        free: Vec::new(),
    };
    match kind {
        // A map-and-fold body stores first, as the scalar loop does
        // (`vec_loop` takes only bodies whose fold comes after the stores).
        VecLoopKind::Reduce { ty, fold, term, stores, .. } => {
            for (array, off, ty, value) in stores {
                let v = cx.value(value)?;
                cx.word(Instruction::VStore, *ty, v, cx.slot(*array), cx.slot(*off));
            }
            let v = cx.value(term)?;
            cx.word(Instruction::VReduce, *ty, d, v, *fold);
        }
        VecLoopKind::Store { stores } => {
            for (array, off, ty, value) in stores {
                let v = cx.value(value)?;
                cx.word(Instruction::VStore, *ty, v, cx.slot(*array), cx.slot(*off));
            }
        }
        // The array is in `d` (moved there in front of the loop).
        VecLoopKind::Push { ty, value, .. } => {
            let v = cx.value(value)?;
            cx.word(Instruction::VStore, *ty | simd::STORE_PUSH, v, d, 0);
        }
    }
    (cx.out.len() <= usize::from(common::simd::LOOP_LEN)).then_some(cx.out)
}

/// A `ByteScan`'s `VScan` body: each stream loaded once into the top
/// vector registers (stream `s` in `v7 - s`), then byte-lane `VBin`s, each
/// tree node into a register counted up from `v0`, so the mask ends in
/// `v0`. A `VLoad`'s index slot holds the stream's offset from the scan
/// position.
pub(crate) fn byte_scan_body(
    expr: &super::inst::LaneExpr,
    streams: &[(ValueId, ValueId)],
    scalars: &[ValueId],
    regs: &[u8],
) -> Option<Vec<Byte>> {
    use super::inst::{LaneExpr, LaneVal};
    use common::simd;

    struct Ctx<'a> {
        out: Vec<Byte>,
        next: u8,
        /// The lowest stream register: nodes stay below it.
        top: u8,
        scalars: &'a [ValueId],
        regs: &'a [u8],
    }

    impl Ctx<'_> {
        fn reg(&mut self) -> Option<u8> {
            let r = self.next;
            if r >= self.top {
                return None;
            }
            self.next += 1;
            Some(r)
        }

        fn vbin(&mut self, kind: u8, dest: u8, a: u8, b: u8) {
            self.out.push(Byte::new(Instruction::VBin).with_dense_abc(kind, dest, a, b));
        }

        fn stream(&self, s: u8) -> Option<u8> {
            let r = (simd::NREGS - 1).checked_sub(usize::from(s))? as u8;
            (r >= self.top).then_some(r)
        }

        fn eval(&mut self, e: &LaneExpr) -> Option<u8> {
            let pred = |op: MirCmpOp| match op {
                MirCmpOp::Lt => dense::CMP_LT,
                MirCmpOp::Le => dense::CMP_LE,
                MirCmpOp::Gt => dense::CMP_GT,
                MirCmpOp::Ge => dense::CMP_GE,
                MirCmpOp::Eq => dense::CMP_EQ,
                MirCmpOp::Ne => dense::CMP_NE,
            };
            let base = self.next;
            let r = match e {
                // `x == x` / `x != x` on a loaded lane.
                LaneExpr::Const(b) => {
                    let (r, x) = (self.reg()?, self.stream(0)?);
                    let k = if *b { dense::CMP_EQ } else { dense::CMP_NE };
                    self.vbin(simd::B_CMP + k, r, x, x);
                    r
                }
                LaneExpr::Cmp { op, lhs, rhs } => match (*lhs, *rhs) {
                    (LaneVal::Stream(a), LaneVal::Stream(b)) => {
                        let (r, x, y) = (self.reg()?, self.stream(a)?, self.stream(b)?);
                        self.vbin(simd::B_CMP + pred(*op), r, x, y);
                        r
                    }
                    (LaneVal::Stream(a), LaneVal::Scalar(k)) => {
                        let (r, x) = (self.reg()?, self.stream(a)?);
                        let slot = self.regs[self.scalars.get(usize::from(k))?.index()];
                        self.vbin(simd::BS_CMP + pred(*op), r, slot, x);
                        r
                    }
                    (LaneVal::Scalar(_), LaneVal::Stream(_)) => {
                        let flipped = match op {
                            MirCmpOp::Lt => MirCmpOp::Gt,
                            MirCmpOp::Le => MirCmpOp::Ge,
                            MirCmpOp::Gt => MirCmpOp::Lt,
                            MirCmpOp::Ge => MirCmpOp::Le,
                            o => *o,
                        };
                        return self.eval(&LaneExpr::Cmp { op: flipped, lhs: *rhs, rhs: *lhs });
                    }
                    (LaneVal::Scalar(_), LaneVal::Scalar(_)) => return None,
                },
                LaneExpr::And(a, b) | LaneExpr::Or(a, b) => {
                    let x = self.eval(a)?;
                    let y = self.eval(b)?;
                    let kind = if matches!(e, LaneExpr::And(..)) { simd::M_AND } else { simd::M_OR };
                    self.vbin(kind, x, x, y);
                    x
                }
                LaneExpr::Not(a) => {
                    let x = self.eval(a)?;
                    self.vbin(simd::M_NOT, x, x, x);
                    x
                }
            };
            // Registers past the result are free again.
            debug_assert_eq!(r, base);
            self.next = base + 1;
            Some(r)
        }
    }

    let top = simd::NREGS.checked_sub(streams.len())? as u8;
    let mut cx = Ctx { out: Vec::new(), next: 0, top, scalars, regs };
    for (s, &(arr, off)) in streams.iter().enumerate() {
        let r = cx.stream(s as u8)?;
        cx.out.push(Byte::new(Instruction::VLoad).with_dense_abc(
            simd::TY_U8,
            r,
            regs[arr.index()],
            regs[off.index()],
        ));
    }
    cx.eval(expr)?;
    (cx.out.len() <= simd::SCAN_MAX_BODY).then_some(cx.out)
}
