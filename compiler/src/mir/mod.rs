//! MIR: typed SSA for every function body, lowered from HIR
//! ([`lower_from_hir`]), and its two emitters: dense 3-address bytecode
//! ([`emit`]) and LIR stack ops ([`emit_lir`]). Passes: CSE (P2), LICM
//! (P6), InstCombine (P7) with float peeps (P11), DestProp (P8), IV
//! strength reduction (P9), cross-block GVN/PRE (P10), saxpy-reduce
//! HostInvoke packs (P12), compiler-only `V*` SIMD (COI-310), and SROA.
//!
//! Dense and LIR `CALL` use the one-word or two-slot typed ABI ([`abi`];
//! COI-291 / B3 / C1). SSA models dest + `dest_hi`; N>2 return words
//! refuse. Typed HostInvoke boxes at the host edge. Allocating bodies
//! carry `Alloc` + `GcBarrier` with live-heap `roots`; their GC frame maps
//! are the precise maps of their final bytecode ([`stackmap`]). Impure HostInvoke / CALL are SSA barriers
//! (I6); LICM hoist uses purity bits. Debugger-attached / `-Og` builds use
//! the same tiers; explicit `Deopt` is skipped at emit. History of each
//! piece: `docs/internals/mir.md`.

mod builder;
mod call_convoy;
mod cse;
mod destprop;
mod deopt;
mod entry;
mod effects;
mod emit;
mod emit_common;
mod error;
mod gc;
mod gates;
mod host_allow;
mod emit_lir;
mod func;
mod inst;
mod instcombine;
mod licm;
mod from_hir;
mod pack;
mod specialize;
mod sccp;
mod sroa;
mod vectorize;
mod strength;
mod stackmap;
mod text;
mod ty;

pub use builder::{MirBuilder, MirError};
pub use cse::{cse, cse_lir, gvn};
pub use destprop::destprop;
#[cfg(test)]
pub use emit_lir::emit_lir;
#[cfg(test)]
pub use entry::LirRefuse;
pub use from_hir::{lower_body as lower_from_hir, opaque_heap as opaque_heap_ty, scalar as scalar_mir_ty, word as word_mir_ty, HirBoundCall, HirCallSite, HirDictOp, HirFfi, HirFfiOperand, HirFnValue, HirIterLoop, HirPolyCall, HirIterNext, HirRetItem, HirRetItems, HirMirInput, HirOpCall, HirParBound, HirParFork, HirParIndex, HirParLoop, HirShow, ScalarLit};
pub use func::MirFunc;
#[cfg(test)]
pub use func::MirBlock;
pub use specialize::{capture_pinned_mir, start_mir_capture, take_mir_capture};
pub use deopt::DraftDeoptMap;
pub use stackmap::is_alloc_opcode;
pub use inst::{MirAllocKind, MirBinOp, MirCastKind, MirConst, MirFfiOp, MirGcKind, MirInst, ValueId};
#[cfg(test)]
pub use inst::{BlockId, LocalId, MirUnaryOp, Terminator};
pub use instcombine::{instcombine, instcombine_lir};
pub use licm::{licm, mark_frozen_arrays};
#[cfg(test)]
pub use error::LowerError;
pub use specialize::{
    fuse_fallback, lower_abi_body_reusing, take_dense_refusal, take_lir_refusal,
    try_specialize_body_side, BodyLabels, BodySidecar, LirDraft,
};
pub use sccp::sccp;
pub use sroa::sroa;
pub use strength::strength_reduce;
#[cfg(test)]
pub use text::{ParseError, parse_func};
pub use ty::MirTy;

/// Build the Mandelbrot inner iteration in typed SSA (acceptance for COI-267).
///
/// Corresponds to the inner `while iter < max_iter` in
/// `examples/perf/mandelbrot.hy` (`zr`/`zi`/`cr`/`ci` are `f64`, `iter` is
/// language `int` → `i64`). No classes.
#[cfg(test)]
pub fn mandelbrot_inner_loop() -> Result<MirFunc, MirError> {
    let mut b = MirBuilder::new("mandel_inner");
    let zr0 = b.add_param(MirTy::F64)?;
    let zi0 = b.add_param(MirTy::F64)?;
    let cr = b.add_param(MirTy::F64)?;
    let ci = b.add_param(MirTy::F64)?;
    let max_iter = b.add_param(MirTy::I64)?;
    let iter0 = b.ins_const(MirConst::I64(0))?;

    const ZR: LocalId = LocalId(0);
    const ZI: LocalId = LocalId(1);
    const ITER: LocalId = LocalId(2);

    let header = b.create_block();
    let body = b.create_block();
    let update = b.create_block();
    let exit = b.create_block();

    b.def_local(ZR, zr0)?;
    b.def_local(ZI, zi0)?;
    b.def_local(ITER, iter0)?;
    b.jump(header)?;

    b.switch_to_block(header);
    let zr = b.use_local(ZR, MirTy::F64)?;
    let zi = b.use_local(ZI, MirTy::F64)?;
    let iter = b.use_local(ITER, MirTy::I64)?;
    let cont = b.ins_cmp(MirCmpOp::Lt, iter, max_iter)?;
    b.branch(cont, body, exit)?;

    b.switch_to_block(body);
    let zr2 = b.ins_binop(MirBinOp::Mul, zr, zr)?;
    let zi2 = b.ins_binop(MirBinOp::Mul, zi, zi)?;
    let mag = b.ins_binop(MirBinOp::Add, zr2, zi2)?;
    let four = b.ins_const(MirConst::f64(4.0))?;
    let escaped = b.ins_cmp(MirCmpOp::Gt, mag, four)?;
    b.branch(escaped, exit, update)?;

    b.switch_to_block(update);
    let tr0 = b.ins_binop(MirBinOp::Sub, zr2, zi2)?;
    let tr = b.ins_binop(MirBinOp::Add, tr0, cr)?;
    let two = b.ins_const(MirConst::f64(2.0))?;
    let t0 = b.ins_binop(MirBinOp::Mul, two, zr)?;
    let t1 = b.ins_binop(MirBinOp::Mul, t0, zi)?;
    let zi_n = b.ins_binop(MirBinOp::Add, t1, ci)?;
    let one = b.ins_const(MirConst::I64(1))?;
    let iter_n = b.ins_binop(MirBinOp::Add, iter, one)?;
    b.def_local(ZR, tr)?;
    b.def_local(ZI, zi_n)?;
    b.def_local(ITER, iter_n)?;
    b.jump(header)?;

    b.switch_to_block(exit);
    let iter_out = b.use_local(ITER, MirTy::I64)?;
    b.set_ret_ty(MirTy::I64);
    b.ret(Some(iter_out))?;

    b.finish()
}

#[cfg(test)]
#[path = "mod.tests.rs"]
mod tests;
