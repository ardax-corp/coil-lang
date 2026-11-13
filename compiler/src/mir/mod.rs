//! MIR: typed SSA for every function body, lowered from HIR
//! ([`lower_from_hir`]). Passes: CSE (P2), LICM
//! (P6), InstCombine (P7) with float peeps (P11), DestProp (P8), IV
//! strength reduction (P9), cross-block GVN/PRE (P10), saxpy-reduce
//! HostInvoke packs (P12), compiler-only `V*` SIMD (COI-310), and SROA.
//!
//! `CALL` uses the one-word or two-slot typed ABI ([`abi`];
//! COI-291 / B3 / C1). SSA models dest + `dest_hi`; N>2 return words
//! refuse. Typed HostInvoke boxes at the host edge. Allocating bodies
//! carry `Alloc` + `GcBarrier` with live-heap `roots`; their GC frame maps
//! are the precise maps of their final code ([`stackmap`]). Impure HostInvoke / CALL are SSA barriers
//! (I6); LICM hoist uses purity bits. Debugger-attached / `-Og` builds use
//! the same tiers. History of each
//! piece: `docs/internals/mir.md`.

mod box_report;
mod builder;
mod bulk;
mod byte_scan;
mod copy_until;
mod vec_loop;
mod cse;
mod destprop;
mod effects;
mod vector_body;
mod error;
mod gc;
mod host_allow;
mod func;
mod inst;
mod instcombine;
mod interchange;
mod licm;
mod from_hir;
mod prefetch;
mod specialize;
pub(crate) use specialize::verify_asked;
mod sccp;
mod sroa;
mod vectorize;
mod bool_thread;
mod phi_sroa;
mod strength;
mod text;
mod ty;

pub use box_report::{BoxKind, BoxSite, start_box_capture, take_box_capture};
pub use builder::{MirBuilder, MirError};
pub use cse::{cse, cse_lir, gvn};
pub use destprop::destprop;
pub use from_hir::{lower_body as lower_from_hir, opaque_heap as opaque_heap_ty, scalar as scalar_mir_ty, word as word_mir_ty, HirBoundCall, HirCallSite, HirDictOp, HirFfi, HirFfiOperand, HirFnValue, HirIterLoop, HirPolyCall, HirIterNext, HirLaneCall, HirLaneElem, HirRetItem, HirRetItems, HirOpCall, HirPacks, HirSite, HirParBound, HirParFork, HirParIndex, HirParLoop, HirShow, LowerCx, PairVariants, ScalarLit, HirFieldFact, HirFieldWord, HirHost, HirLaneUses, HirNew, LocalFrame, LocalWords};
pub use func::MirFunc;
pub use gc::liveness;
pub use func::{MirBlock, is_tail_call_inst};
pub use inst::{MirAllocKind, MirBinOp, MirCastKind, MirConst, MirFfiOp, MirGcKind, MirInst, MirLaneShape, ValueId};
pub use inst::{LaneExpr, LaneVal, MirCmpOp, VecExpr, VecLoopKind};
pub use inst::{BlockId, MirUnaryOp, Terminator};
#[cfg(test)]
pub use inst::LocalId;
pub mod archive;
mod frame_words;
mod int_range;
pub use int_range::in_range_steps;
mod native_alloc;
pub use native_alloc::{AllocCall, alloc_call};
mod native_capture;
pub mod coverage;
pub mod native_expand;
pub use frame_words::{allocates, barrier_pairs, heap_word, held_words, is_generator, numeric, opaque_words, pointer_words, root_handles, scalar, ty_of, used_params, word_kind, pair_words};
pub(crate) use native_capture::capture_open;
pub use native_capture::{ComptimeCapture, ConstArg, ConstCall, ConstResult, NativeBody, VectorOp, ffi_use, capture_const_calls, replace_const_calls, capture_native_bodies, vector_op, peek_native_capture, start_native_capture, start_raw_capture, take_native_capture, without_native_capture};
pub use instcombine::{instcombine, };
pub use licm::{licm, mark_frozen_arrays, spin_headers};
pub use sccp::sccp;
pub use sroa::sroa;
pub use bool_thread::thread_bool_phis;
pub use phi_sroa::phi_sroa;
pub use bulk::bulk_push;
pub use byte_scan::byte_scan;
pub use copy_until::copy_until;
pub use vec_loop::vec_loops;
pub use prefetch::mark_prefetch_scans;
pub use interchange::interchange_loops;
pub use strength::strength_reduce;
#[cfg(test)]
pub use text::parse_func;
pub use ty::MirTy;

/// Build the Mandelbrot inner iteration in typed SSA (acceptance for COI-267).
///
/// Corresponds to the inner `while iter < max_iter` in
/// `examples/perf/mandelbrot.coil` (`zr`/`zi`/`cr`/`ci` are `f64`, `iter` is
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
