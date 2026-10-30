//! Emit a body's MIR (lowered from HIR) as dense bytecode when that wins,
//! else as LIR stack ops.

use common::Instruction;

use crate::il::opt::OptimizeOptions;
use crate::il::{IlOp, Label};

use std::collections::HashMap;

use crate::layout::{DenseAbi, DenseCallMap};
use super::deopt::DraftDeoptMap;
use super::emit::emit_dense;

/// C3 sidecar from a dense / LIR replace (named lets + deopt maps).
#[derive(Clone, Debug, Default)]
pub struct BodySidecar {
    pub debug_slot_remap: HashMap<u32, u32>,
    pub deopt: Option<DraftDeoptMap>,
    /// Allocating dense body: its registers may sit past the cursor, so its
    /// frame needs a decoded extent (see `precise_frames`).
    pub needs_frame_extent: bool,
    /// A LIR body's unwind cleanup pad, emitted after it: its label and the
    /// frame words it reads ([`super::MirFunc::cleanup`]).
    pub cleanup_pad: Option<(Label, u32)>,
    /// A dense body's panic write-backs ([`super::emit::PanicPads`]).
    pub panic_pads: super::emit::PanicPads,
}

type RefusalSlot = std::thread::LocalKey<std::cell::RefCell<Option<String>>>;

/// `(function, "dense" | "lir", MIR text)` captured for `coil dissect --mir`.
pub type CapturedMir = Vec<(String, &'static str, String)>;

thread_local! {
    /// `coil dissect --mir`: `(function, form, MIR text)` of each body that
    /// reached emission. `None` when not capturing.
    static MIR_CAPTURE: std::cell::RefCell<Option<CapturedMir>> =
        const { std::cell::RefCell::new(None) };
    static DENSE_REFUSAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static LIR_REFUSAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Start recording the final MIR of each specialized body (dissect).
pub fn start_mir_capture() {
    MIR_CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Stop recording and return `(function, "dense" | "lir", MIR text)`.
pub fn take_mir_capture() -> CapturedMir {
    MIR_CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default())
}

/// A pinned (`defer`) body's MIR from HIR, which no tier emits yet.
pub fn capture_pinned_mir(name: &str, func: &crate::mir::MirFunc) {
    capture_mir(name, "pinned", func);
}

fn capture_mir(name: &str, form: &'static str, func: &crate::mir::MirFunc) {
    MIR_CAPTURE.with(|c| {
        if let Some(list) = c.borrow_mut().as_mut() {
            list.push((name.to_string(), form, func.to_string()));
        }
    });
}

/// Why the last [`try_specialize_body_side`] returned `None` (keep-rate census).
pub fn take_dense_refusal() -> Option<String> {
    DENSE_REFUSAL.with(|c| c.borrow_mut().take())
}

/// Why the last [`try_lower_abi_body_side`] returned `None` (keep-rate census).
pub fn take_lir_refusal() -> Option<String> {
    LIR_REFUSAL.with(|c| c.borrow_mut().take())
}

/// Record a coarse refusal key (digits folded) in `slot` and return `None`.
fn note_refusal<T>(slot: &'static RefusalSlot, why: impl std::fmt::Display) -> Option<T> {
    let key: String = why
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .take(60)
        .collect();
    slot.with(|c| *c.borrow_mut() = Some(key));
    None
}

fn refuse_dense<T>(why: impl std::fmt::Display) -> Option<T> {
    note_refusal(&DENSE_REFUSAL, why)
}

fn refuse<T>(why: impl std::fmt::Display) -> Option<T> {
    note_refusal(&LIR_REFUSAL, why)
}

/// The labels a body binds: its entry and the aliases a call may name.
/// Read off the `Label` ops alone, never the body's code.
#[derive(Clone, Debug, Default)]
pub struct BodyLabels {
    /// First label bound anywhere in the body.
    pub first: Option<Label>,
    /// The labels leading the body.
    pub head: Vec<Label>,
}

impl BodyLabels {
    pub fn of(ops: &[IlOp]) -> Self {
        let label = |op: &IlOp| match op {
            IlOp::Label(l) => Some(*l),
            _ => None,
        };
        Self {
            first: ops.iter().find_map(label),
            head: ops.iter().map_while(label).collect(),
        }
    }
}

fn count_mir(func: &crate::mir::MirFunc, pred: impl Fn(&crate::mir::MirInst) -> bool) -> usize {
    func.blocks.iter().flat_map(|b| b.insts.iter()).filter(|i| pred(i)).count()
}

fn is_mir_store_index(i: &crate::mir::MirInst) -> bool {
    matches!(i, crate::mir::MirInst::StoreIndex { .. })
}

fn is_mir_index(i: &crate::mir::MirInst) -> bool {
    matches!(i, crate::mir::MirInst::Index { .. })
}

/// `official_entry` is `IlFunc.meta.entry` (CALL target). New labels must
/// not reuse that id or concat lands calls on a loop header.
///
/// Dense-specialize one body from `from_hir` (MIR lowered from HIR):
/// dense IL plus its ABI when it beats `fallback`, the body's LIR form.
///
/// CSE → LICM → CSE → InstCombine (incl. P11 float peeps) → DestProp → SR → CSE → GVN/PRE,
/// then saxpy-reduce HostInvoke (P12) or dense emit (W4 allowlisted
/// HostInvoke edges box → call → unbox; COI-291 dense→dense `CALL` when
/// `calls` lists the callee).
#[allow(clippy::too_many_arguments)]
pub fn try_specialize_body_side(
    fallback: Option<&[IlOp]>,
    labels: &BodyLabels,
    name: &str,
    pool: &mut Vec<u64>,
    calls: &DenseCallMap,
    official_entry: Option<Label>,
    from_hir: Option<&crate::mir::MirFunc>,
    side: &mut BodySidecar,
    opts: &OptimizeOptions,
) -> Option<(Vec<IlOp>, DenseAbi)> {
    let Some(func) = from_hir else {
        return refuse_dense("no MIR from HIR");
    };
    if let Some(why) = func.emit_refusal {
        return refuse_dense(why);
    }
    if !opts.mir_dense {
        return refuse_dense("dense tier off");
    }
    let mut direct_side = BodySidecar::default();
    let mut direct_pool = pool.clone();
    let out = specialize_side(fallback, labels, name, &mut direct_pool, calls, official_entry, func, &mut direct_side, opts)?;
    *pool = direct_pool;
    *side = direct_side;
    crate::il::opt::note_hir_mir_kept();
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn specialize_side(
    fallback: Option<&[IlOp]>,
    labels: &BodyLabels,
    name: &str,
    pool: &mut Vec<u64>,
    calls: &DenseCallMap,
    official_entry: Option<Label>,
    from_hir: &crate::mir::MirFunc,
    side: &mut BodySidecar,
    opts: &OptimizeOptions,
) -> Option<(Vec<IlOp>, DenseAbi)> {
    // Nested / multi-header numeric loops are eligible (flagship mandelbrot).
    // Infer requires float +/−/×/÷, or counted i64 +/−/×/÷/%. A
    // back-edge or a straight-line numeric body may lift, and dense keeps
    // every body it can emit (no cost compare against LIR). S3/S3b: one-word CALL (dense map or
    // open), I6 HostInvoke including Q9 R2 `from_bytes` / `to_bytes`, heap
    // index / ArrayLen / StoreIndex (dense residuals after V*). FORMAT /
    // STRING / PRINT / STRINGIFY stay off dense (Q9 R1 is MIR→LIR; R3
    // maps FORMAT / STRINGIFY). Q6
    // counted `for` is i64 + index.
    // Q8: niche / two-slot match may dense. D3: boxed JumpIfMatch /
    // multi-payload Unpack may dense (stack JIM + per-index MatchPayload). Alloc / InitTyped
    // take dense with a frame extent (minor 22). S2d: mapped *preheader*
    // Make* + index loop may take dense. A2: Index / Make* / ArrayLen /
    // StoreIndex emit dense-native. D2: heap LoadField / GetField /
    // SetField emit DenseField*; Object Alloc is DenseMakeObject.
    // CALL / HostInvoke box at the ABI
    // edge via DensePush. S2l: try dense so SROA / LICM can delete work;
    // keep residual in-loop Make* only when reconstruct is dense-native
    // and cost ≤ fuse-IL. Post-loop-only `return [x]` stays fuse-IL
    // (COI-87 invert+fuse). Debugger-attached / -Og still enter (B8).
    // S2k: last-arm writes must survive; Seek size is a cost, not a cap.
    // B7 / C1: sibling / mutual TailCall and self multi-word CALL/RETURN
    // may lift (stack-arg protocol, reserved callee entry labels). N>2
    // still refuses at infer/lower.
    let facts = super::gates::GateFacts::of(from_hir);
    let has_alloc = facts.has_alloc;
    let inloop_alloc = facts.alloc_in_loop;
    if facts.post_loop_alloc_return {
        return refuse_dense("post-loop alloc return");
    }
    // Registers may sit past the cursor at a safepoint, and generic host
    // results are heap words typed `i64`: a conservatively scanned frame
    // covers the whole extent.
    side.needs_frame_extent = has_alloc;
    // A coverage compile counts a line by the code it emits; dense's MIR
    // passes fold a line's constants into the next one (#1494). LIR keeps
    // each line's code there (`keep_lines`).
    if from_hir.keep_lines {
        return refuse_dense("coverage");
    }
    if !calls_agree(from_hir, calls) {
        return refuse_dense("call ABI");
    }
    let mut func = from_hir.clone();
    // `-O0` / `-Og` run none of these; `-O1` only the folds.
    if opts.mir_fold {
        crate::mir::sccp(&mut func);
        check_after(&func, "sccp");
    }
    if opts.mir_loop {
        loop_opts(&mut func, true);
        // A nest walking memory across its rows walks along them instead;
        // the inner loop's row base is then invariant: hoist it.
        if crate::mir::interchange_loops(&mut func) > 0 {
            check_after(&func, "interchange_loops");
            loop_opts(&mut func, true);
        }
    }
    if opts.mir_fold {
        crate::mir::instcombine(&mut func);
        check_after(&func, "instcombine");
        crate::mir::destprop(&mut func);
        check_after(&func, "destprop");
    }
    if opts.mir_loop {
        crate::mir::strength_reduce(&mut func);
        check_after(&func, "strength_reduce");
        crate::mir::cse(&mut func);
        check_after(&func, "cse");
        crate::mir::gvn(&mut func);
        check_after(&func, "gvn");
    }
    // S2f: reuse the mutated array after StoreIndex (drop rematerialized Alloc).
    crate::mir::sroa(&mut func);
    check_after(&func, "sroa");
    // An object made on each path into a φ and only read field by field
    // after it (`find_in(..) ?? new Span(..)`) is its fields' φs.
    crate::mir::phi_sroa(&mut func);
    check_after(&func, "phi_sroa");
    // `a && b` joins on a `bool` φ: each way in branches for itself.
    if opts.mir_fold && crate::mir::thread_bool_phis(&mut func) > 0 {
        check_after(&func, "thread_bool_phis");
    }
    // A loop that pushes each byte until one stops it: a scan for the
    // stop (which `byte_scan` takes next) and one copy of the run.
    if opts.mir_loop && crate::mir::copy_until(&mut func) > 0 {
        check_after(&func, "copy_until");
    }
    // Early-exit byte loops skip 64 no-op iterations at a time. Their
    // range setup is invariant in any outer loop: hoist it.
    if opts.mir_loop && crate::mir::byte_scan(&mut func) > 0 {
        loop_opts(&mut func, true);
        check_after(&func, "byte_scan");
    }
    // Counted copy and fill loops run as one call.
    if opts.mir_loop && crate::mir::bulk_push(&mut func) > 0 {
        check_after(&func, "bulk_push");
    }
    // Counted map and reduce loops run 8 lanes at a time in one op.
    if opts.mir_loop && crate::mir::vec_loops(&mut func) > 0 {
        check_after(&func, "vec_loops");
    }
    // A field a loop writes stays in a register across it; last, so no
    // pass moves code across the stores a panic owes the heap.
    if opts.mir_loop && crate::mir::promote_fields(&mut func) > 0 {
        check_after(&func, "promote_fields");
    }
    // Hints only: forward scans of pointer arrays prefetch ahead.
    if opts.mir_loop {
        crate::mir::mark_prefetch_scans(&mut func);
    }
    super::deopt::fill_deopt_maps(&mut func);
    paint_index_dest_from_uses(&mut func);
    let stores_ssa = func
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter(|i| matches!(i, crate::mir::MirInst::StoreIndex { .. }))
        .count();
    // Heap writes and reads the source holds: the MIR its LIR comes from.
    let (src_stores, src_index) = (count_mir(from_hir, is_mir_store_index), count_mir(from_hir, is_mir_index));
    if src_stores > 0 && stores_ssa == 0 {
        return refuse_dense("store index dropped in SSA");
    }
    // The body's params are its MIR's.
    let Some(abi) = DenseAbi::from_func(&func) else {
        return refuse_dense("dense abi");
    };
    let entry = official_entry.or(labels.first);
    let (remap, deopt) = super::emit::dense_sidecars(&func, entry);
    side.debug_slot_remap = remap;
    side.deopt = Some(deopt);
    if opts.mir_loop
        && let Some(packed) = super::pack::try_axpy_pack(&func, entry, pool)
    {
        return Some((packed, abi));
    }
    capture_mir(name, "dense", &func);
    let out = match emit_dense(&func, entry, pool, has_alloc) {
        Ok((o, pads)) => {
            side.panic_pads = pads;
            o
        }
        Err(e) => return refuse_dense(format!("emit: {e}")),
    };
    // Heap writes have no SSA users; refuse if reconstruct dropped one.
    if count_store_index(&out) < fallback.map_or(src_stores, count_store_index) {
        return refuse_dense("store index dropped in emit");
    }
    // Const-fold must not erase every Index (for-in / invert+fuse leftover).
    if fallback.map_or(src_index, count_index) > 0 && count_index(&out) == 0 {
        return refuse_dense("index erased");
    }
    // In-loop Make*: keep dense only when heap ops are native (S2l / A2).
    if inloop_alloc
        && has_alloc_inside_loop(&out)
        && residual_heap_box(&out)
    {
        return refuse_dense("boxed in-loop alloc");
    }
    // The compare is against the code that runs if dense refuses (its
    // LIR): a body the LIR tier refuses has nothing to lose to.
    let Some(ops) = fallback else {
        return Some((out, abi));
    };
    // Two-slot CALL/RETURN must not grow a boxed reconstruct (C1).
    if count_make_enum(&out) > count_make_enum(ops) {
        return refuse_dense("MakeEnum growth");
    }
    Some((out, abi))
}

/// Every `CALL` in `func` uses its callee's dense ABI, when the callee has one.
/// The callee may declare more entry words than the call passes: dictionary
/// slots past its parameters (heap words, read only where a dictionary
/// program loads one), which the LIR `CALL` leaves unset too.
fn calls_agree(func: &crate::mir::MirFunc, calls: &DenseCallMap) -> bool {
    use crate::mir::{MirInst, MirTy};
    func.blocks.iter().flat_map(|b| b.insts.iter()).all(|i| match i {
        MirInst::Call { dest, dest_hi, target, args } => calls.get(&target.0).is_none_or(|abi| {
            abi.ret_hi == dest_hi.map(|h| func.ty(h))
                && abi.ret == func.ty(*dest)
                && abi.params.len() >= args.len()
                && abi.params[args.len()..].iter().all(|&p| p == MirTy::HeapRef)
                && abi.params.iter().zip(args).all(|(&p, &a)| p == func.ty(a))
        }),
        _ => true,
    })
}

/// Index / open CALL dests default to i64 when the next IL is StorePop.
/// Repaint from float uses so DenseBin / RETURN keep the Value bits.
fn paint_index_dest_from_uses(func: &mut crate::mir::func::MirFunc) {
    use crate::mir::inst::{MirInst, ValueId};
    let mut paint = Vec::new();
    for b in &func.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Index { dest, .. } | MirInst::Call { dest, .. } => {
                    paint.push(*dest);
                }
                _ => {}
            }
        }
    }
    if paint.is_empty() {
        return;
    }
    let mut ty_of = vec![None; func.types.len()];
    for b in &func.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Bin { ty, lhs, rhs, .. } if ty.is_float() => {
                    ty_of[lhs.index()] = Some(*ty);
                    ty_of[rhs.index()] = Some(*ty);
                }
                MirInst::StoreIndex { value, .. } => {
                    let vt = func.ty(*value);
                    if vt.is_float() {
                        ty_of[value.index()] = Some(vt);
                    }
                }
                _ => {}
            }
        }
    }
    for ValueId(id) in paint {
        if let Some(ty) = ty_of.get(id as usize).copied().flatten() {
            func.types[id as usize] = ty;
        }
    }
}

fn count_make_enum(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::MakeEnum { .. } => true,
            IlOp::Byte { byte, .. } => {
                matches!(
                    *byte.bytecode(),
                    Instruction::MakeEnum
                        | Instruction::MakeEnumReturn
                        | Instruction::MakeEnumK
                        | Instruction::MakeEnumReturnK
                )
            }
            _ => false,
        })
        .count()
}

fn count_store_index(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::Byte { byte, .. } => matches!(
                *byte.bytecode(),
                Instruction::StoreIndex
                    | Instruction::StoreIndexUnchecked
                    | Instruction::StoreIndexPin
                    | Instruction::StoreIndexPinUnchecked
                    | Instruction::DenseStoreIndex
            ),
            _ => false,
        })
        .count()
}

fn count_index(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::Index { .. } | IlOp::IndexUnchecked { .. } => true,
            IlOp::Byte { byte, .. } => matches!(
                *byte.bytecode(),
                Instruction::Index | Instruction::IndexUnchecked | Instruction::IndexPin
                    | Instruction::IndexPinUnchecked | Instruction::DenseIndex
                    | Instruction::DenseIndexJmpf
            ),
            _ => false,
        })
        .count()
}

fn is_stack_heap_op(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::Index
            | Instruction::IndexUnchecked
            | Instruction::StoreIndex
            | Instruction::StoreIndexUnchecked
            | Instruction::ArrayLen
            | Instruction::MakeArray
            | Instruction::MakeArrayK
            | Instruction::MakeTuple
            | Instruction::MakeTupleK
            | Instruction::MakeEnum
            | Instruction::MakeEnumK
            | Instruction::MakeEnumReturn
            | Instruction::MakeEnumReturnK
            | Instruction::InitTyped
    )
}

/// LOAD / StorePop around stack Index / Make* — the A2 tax we refuse to ship.
fn residual_heap_box(ops: &[IlOp]) -> bool {
    ops.iter().any(|op| match op {
        IlOp::Index { .. }
        | IlOp::IndexUnchecked { .. }
        | IlOp::MakeArray { .. }
        | IlOp::MakeTuple { .. }
        | IlOp::MakeEnum { .. } => true,
        IlOp::Byte { byte, .. } => is_stack_heap_op(*byte.bytecode()),
        _ => false,
    })
}

/// MIR→LIR for a body dense does not keep: `from_hir` (MIR lowered from
/// HIR) emitted as stack ops with `emit_lir` keeping single-use values on
/// the stack. The body reads its walls off its MIR.
pub fn try_lower_abi_body_side(
    labels: &BodyLabels,
    name: &str,
    pool: &mut Vec<u64>,
    from_hir: Option<&crate::mir::MirFunc>,
    side: &mut BodySidecar,
    opts: &OptimizeOptions,
) -> Option<Vec<IlOp>> {
    // I8: any inferable unfused body, not only two-slot / match / field accidents.
    // S2c: allocating leftovers need every allocation followed by its barrier.
    // S2d: mapped in-loop / preheader Make* may reconstruct; post-loop-only
    // `return [x]` stays fuse-IL so invert+fuse (COI-87) remains.
    // An argless `FORMAT` is no `refuses_alloc` site but still a mapped
    // allocation: the body needs its maps all the same.
    let has_alloc = from_hir.is_some_and(super::gates::maps_allocation);
    // MIR lowered from HIR maps its own allocations (`lir_tail` builds the
    // body's draft from it).
    let Some(direct) = from_hir else {
        return refuse("no MIR from HIR");
    };
    if let Some(why) = direct.emit_refusal {
        return refuse(why);
    }
    if has_alloc && !super::stackmap::mir_maps_ok(direct) {
        return refuse("no MIR maps");
    }
    if let Some(wall) = super::gates::fuse_lir_wall(direct, has_alloc) {
        return refuse(format!("lir wall {wall:?}"));
    }
    let mut func = direct.clone();
    // A debugger reads the locals where the source keeps them, and a
    // coroutine's frame lives across its yields: both keep their code.
    let yields = func.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, crate::mir::MirInst::Yield { .. }));
    if !func.debug_keep && !yields && opts.mir_loop {
        loop_opts(&mut func, false);
        if crate::mir::bulk_push(&mut func) > 0 {
            check_after(&func, "bulk_push");
        }
    }
    if has_alloc {
        crate::mir::sroa(&mut func);
        check_after(&func, "sroa");
    }
    lir_tail(labels, name, pool, has_alloc, func, side, opts)
}

/// The scalar passes both tiers run first: local CSE, trivial-φ copy
/// forwarding (a loop-carried copy of an invariant reads as invariant),
/// loop-invariant code motion, then CSE again over what moved together.
/// `dense`: the body emits dense ([`crate::mir::licm`]).
fn loop_opts(func: &mut crate::mir::MirFunc, dense: bool) {
    let cse = if dense { crate::mir::cse } else { crate::mir::cse_lir };
    cse(func);
    check_after(func, "cse");
    crate::mir::destprop(func);
    check_after(func, "destprop");
    crate::mir::licm(func, dense);
    check_after(func, "licm");
    cse(func);
    check_after(func, "cse");
}

/// Debug builds verify the MIR after each pass, so a pass that breaks SSA
/// is named where it does instead of surfacing as a bad emit (#1869).
#[cfg(debug_assertions)]
fn check_after(func: &crate::mir::MirFunc, pass: &str) {
    if let Err(e) = func.verify().and_then(|()| func.verify_dominance()) {
        panic!("MIR verify failed after {pass} in {}: {e}", func.name);
    }
}

#[cfg(not(debug_assertions))]
fn check_after(_: &crate::mir::MirFunc, _: &str) {}

/// A body's LIR, lowered once from its MIR by [`fuse_fallback`] and kept
/// for [`lower_abi_body_reusing`], with what that lowering left behind.
pub struct LirDraft {
    /// The LIR (`None`: the LIR tier refused the body).
    out: Option<Vec<IlOp>>,
    /// Length of the pool it was lowered against.
    base: usize,
    /// The constants it added to that pool, in the order it added them.
    added: Vec<u64>,
    side: BodySidecar,
    /// Its MIR capture (`None`: not capturing).
    captured: Option<CapturedMir>,
    refusal: Option<String>,
}

impl LirDraft {
    /// The stack ops the LIR tier would emit: what runs if dense refuses
    /// the body, for the dense gates to compare against.
    pub fn ops(&self) -> Option<&[IlOp]> {
        self.out.as_deref()
    }
}

/// Lower a body's MIR to LIR against a copy of `pool`: what runs if dense
/// refuses it. Leaves `pool` and the MIR capture as they were and the
/// refusal census empty; [`lower_abi_body_reusing`] keeps the result.
pub fn fuse_fallback(labels: &BodyLabels, name: &str, pool: &[u64], func: &crate::mir::MirFunc, opts: &OptimizeOptions) -> LirDraft {
    let outer = MIR_CAPTURE.with(|c| {
        let mut c = c.borrow_mut();
        let outer = c.take();
        if outer.is_some() {
            *c = Some(Vec::new());
        }
        outer
    });
    LIR_REFUSAL.with(|c| c.borrow_mut().take());
    let mut grown = pool.to_vec();
    let mut side = BodySidecar::default();
    let out = try_lower_abi_body_side(labels, name, &mut grown, Some(func), &mut side, opts);
    let captured = MIR_CAPTURE.with(|c| std::mem::replace(&mut *c.borrow_mut(), outer));
    let refusal = LIR_REFUSAL.with(|c| c.borrow_mut().take());
    LirDraft {
        out,
        base: pool.len(),
        added: grown.split_off(pool.len()),
        side,
        captured,
        refusal,
    }
}

/// [`try_lower_abi_body_side`] of the body `draft` was lowered from,
/// against `pool`, reusing `draft` instead of lowering again. The module
/// pool only grows (every writer interns), so `pool` still starts with the
/// pool the draft saw: a constant found there keeps its index, and
/// interning the constants it added, in order, gives each the index a
/// fresh lowering would and grows `pool` the same way. Lowers afresh
/// when there is no draft (or the pool could overflow an index).
pub fn lower_abi_body_reusing(
    draft: Option<LirDraft>,
    labels: &BodyLabels,
    name: &str,
    pool: &mut Vec<u64>,
    from_hir: Option<&crate::mir::MirFunc>,
    side: &mut BodySidecar,
    opts: &OptimizeOptions,
) -> Option<Vec<IlOp>> {
    let fits = |d: &LirDraft| pool.len() >= d.base && u32::try_from(pool.len() + d.added.len()).is_ok();
    let Some(draft) = draft.filter(fits) else {
        return try_lower_abi_body_side(labels, name, pool, from_hir, side, opts);
    };
    let at: Vec<u32> = draft
        .added
        .iter()
        .map(|&bits| {
            let i = pool.iter().position(|&x| x == bits).unwrap_or_else(|| {
                pool.push(bits);
                pool.len() - 1
            });
            i as u32
        })
        .collect();
    if let Some(list) = draft.captured {
        MIR_CAPTURE.with(|c| {
            if let Some(capture) = c.borrow_mut().as_mut() {
                capture.extend(list);
            }
        });
    }
    if let Some(why) = draft.refusal {
        LIR_REFUSAL.with(|c| *c.borrow_mut() = Some(why));
    }
    *side = draft.side;
    let mut out = draft.out?;
    for op in &mut out {
        if let IlOp::ConstPool { idx, .. } = op
            && let Some(k) = (*idx as usize).checked_sub(draft.base)
        {
            *idx = at[k];
        }
    }
    Some(out)
}

/// Emit the LIR tier's stack IL from `func`.
#[allow(clippy::too_many_arguments)]
fn lir_tail(
    labels: &BodyLabels,
    name: &str,
    pool: &mut Vec<u64>,
    has_alloc: bool,
    mut func: crate::mir::MirFunc,
    side: &mut BodySidecar,
    opts: &OptimizeOptions,
) -> Option<Vec<IlOp>> {
    // Constants that only meet in MIR (after inlining, copy forwarding)
    // fold as they do on dense, whether or not the body has the arithmetic
    // dense pays for (#1434), at `-O1` and up (`mir_fold`). A folded
    // branch leaves blocks nothing reaches: drop them, as lowering does
    // (emit_lir's liveness is slow over them). A coverage compile keeps
    // each line's code, so a line whose value folds still counts as run.
    // SCCP first: it folds through φs (a join whose other arms never run,
    // a loop φ that keeps its value), which `instcombine_lir` skips.
    let fold = !func.keep_lines && opts.mir_fold;
    if fold {
        crate::mir::sccp(&mut func);
        check_after(&func, "sccp");
    }
    if fold && crate::mir::instcombine_lir(&mut func) > 0 {
        let order: Vec<_> = func.blocks.iter().map(|b| b.id).collect();
        super::from_hir::tidy(&mut func, &order);
        check_after(&func, "instcombine_lir");
    }
    super::deopt::fill_deopt_maps(&mut func);
    let entry = labels.first;
    // Every label leading the body binds the entry; a self call may name any.
    let aliases: Vec<Label> = labels.head.iter().copied().filter(|&l| Some(l) != entry).collect();
    capture_mir(name, "lir", &func);
    // Emission plans the frame once; the sidecars read it (#1502).
    let (mut out, lir_frame) = match super::emit_lir::emit_lir_framed(&func, entry, &aliases, pool, has_alloc) {
        Ok(o) => o,
        Err(e) => {
            let (remap, deopt) = super::emit_lir::lir_sidecars(&func);
            side.debug_slot_remap = remap;
            side.deopt = Some(deopt);
            return refuse(format!("emit: {e}"));
        }
    };
    let (remap, deopt) = lir_frame.sidecars(&func);
    side.debug_slot_remap = remap;
    side.deopt = Some(deopt);
    if let Some(pad) = func.cleanup.as_deref() {
        let (label, frame, ops) = match emit_cleanup_pad(lir_frame.home_frame(), pad, &out, pool) {
            Ok(p) => p,
            Err(why) => return refuse(why),
        };
        out.extend(ops);
        side.cleanup_pad = Some((label, frame));
    }
    Some(out)
}

/// The LIR of `func`'s cleanup `pad`, to follow `body` (`func`'s LIR): its
/// label (past every label `body` names), the frame words of `func` it
/// reads, and its ops. Its own registers sit past that frame.
fn emit_cleanup_pad(
    (base, frame): (u8, u32),
    pad: &crate::mir::MirFunc,
    body: &[IlOp],
    pool: &mut Vec<u64>,
) -> Result<(Label, u32, Vec<IlOp>), String> {
    let mut pad = pad.clone();
    pad.home_base = Some(base);
    let words: Vec<crate::mir::ValueId> = (0..frame)
        .map(|_| {
            pad.types.push(crate::mir::MirTy::I64);
            crate::mir::ValueId(pad.types.len() as u32 - 1)
        })
        .collect();
    pad.params = words;
    let top = body
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(l) => Some(l.0),
            IlOp::Entry { target, .. } | IlOp::Jump { target, .. } => Some(target.0),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let label = Label(top + 1);
    let mapped = pad.has_mapped_gc_edge();
    let ops = super::emit_lir::emit_lir(&pad, Some(label), pool, mapped).map_err(|e| format!("cleanup pad: {e}"))?;
    Ok((label, frame, ops))
}

/// Make* / InitTyped between a loop header and its back-edge.
/// Preheader alloc plus an index loop is not this — S2d may specialize those.
fn has_alloc_inside_loop(ops: &[IlOp]) -> bool {
    let loops = loop_ranges(ops);
    if loops.is_empty() {
        return false;
    }
    ops.iter().enumerate().any(|(i, op)| {
        super::gc::refuse_reason(op).is_some() && loops.iter().any(|&(h, j)| h <= i && i < j)
    })
}

fn loop_ranges(ops: &[IlOp]) -> Vec<(usize, usize)> {
    let mut label_at = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) = op {
            label_at.entry(*id).or_insert(i);
        }
    }
    let mut ranges = Vec::new();
    for (j, op) in ops.iter().enumerate() {
        if let IlOp::Jump {
            target: Label(id), ..
        } = op
            && let Some(&h) = label_at.get(id)
                && h < j {
                    ranges.push((h, j));
                }
    }
    ranges
}

#[cfg(test)]
mod cost_gate_tests {
    use common::{Byte, DebugLoc, Instruction};

    use crate::il::IlOp;

    use super::residual_heap_box;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn residual_make_counts_as_boxed() {
        let ops = [IlOp::MakeArray { elem_kind: 0, arity: 2, loc: loc() }];
        assert!(residual_heap_box(&ops));
        let native = [IlOp::byte(
            Byte::new(Instruction::DenseMake).with_dense_abc(0, 1, 2, 3),
        )];
        assert!(!residual_heap_box(&native));
    }

    #[test]
    fn body_labels_read_only_the_labels() {
        use crate::il::Label;
        let ops = [
            IlOp::Label(Label(4)),
            IlOp::Label(Label(9)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Label(Label(12)),
        ];
        let labels = super::BodyLabels::of(&ops);
        assert_eq!(labels.first, Some(Label(4)));
        assert_eq!(labels.head, vec![Label(4), Label(9)]);
        assert_eq!(super::BodyLabels::of(&ops[2..]).first, Some(Label(12)));
        assert!(super::BodyLabels::of(&ops[2..]).head.is_empty());
    }

    /// A pass that leaves broken MIR is named in debug builds (#1869).
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "MIR verify failed after sroa in broken")]
    fn check_after_names_the_pass() {
        super::check_after(&crate::mir::MirFunc::new("broken"), "sroa");
    }

    /// A use one arm defines but the join reads passes the structural
    /// check; the dominance check catches it (#1869).
    #[test]
    fn verify_dominance_rejects_a_use_one_arm_defines() {
        let src = r#"
func @arm(v0: i64) -> i64 {
bb0:
    v1 = iconst.i64 0
    v2 = icmp.slt v0, v1
    brif v2, bb1, bb2
bb1:
    v3 = iadd v0, v0
    jump bb2
bb2:
    v4 = iadd v3, v0
    return v4
}
"#;
        let f = crate::mir::parse_func(src).expect(src);
        f.verify().unwrap();
        let err = f.verify_dominance().unwrap_err();
        assert!(err.contains("reads v3"), "{err}");
        let ok = crate::mir::parse_func(&src.replace("v4 = iadd v3, v0", "v4 = iadd v0, v0")).unwrap();
        ok.verify_dominance().unwrap();
    }
}
