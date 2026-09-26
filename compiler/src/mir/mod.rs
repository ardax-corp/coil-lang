//! Numeric MIR: type lattice + SSA builder (P0), dense emit (P1), CSE (P2),
//! Result/Option MIR→LIR (P3 / COI-270), LICM (P6 / COI-280),
//! InstCombine (P7 / COI-281), DestProp (P8 / COI-282),
//! IV strength reduction (P9 / COI-283), cross-block GVN/PRE (P10 / COI-284),
//! conservative float peeps (P11 / COI-285), saxpy-reduce HostInvoke
//! packs (P12 / COI-286), compiler-only `V*` SIMD (COI-310), I1 heap/niche `MirTy` names (COI-293), and
//! I2 match / `JumpIfMatch` on niche, two-slot, and boxed-overlap payloads
//! (COI-294 / COI-302), and
//! I3 field load/store on non-escaping unboxed class locals (COI-295), and
//! I4 / Q9 string / format SSA (COI-296 / COI-332) as a delivery ladder, and
//! I5 alloc / GC-barrier edges (COI-300) with S2a live-root
//! sidecar (COI-305) and S2b slot / frame maps (COI-306),
//! I6 HostInvoke / CALL effect edges from the purity sidecar (COI-297), and
//! I7 debugger / deopt boundaries on MIR edges (COI-299), and
//! I8 broadened MIR emit entry (COI-298).
//!
//! Specialized numeric loops lower to dense 3-address opcodes. Leftover
//! bodies that [`entry`] accepts lift through MIR→LIR (`RETURN` width 1 or
//! 2). Dense / LIR `CALL` uses the one-word or two-slot typed ABI
//! ([`abi`]; COI-291 / B3 / C1). SSA models dest + `dest_hi`; N>2 refuses
//! at that cap (same field, extra dests later).
//! Typed HostInvoke still boxes at the host edge. Escaping / heap-backed
//! named class locals stay on [`crate::il`]. `FORMAT` / `STRING` /
//! `STRINGIFY` / `PRINT` may enter MIR→LIR (I4 / Q9 R1); dense infer
//! still refuses those table ops. `FORMAT` / `STRINGIFY` take I5-style
//! maps (Q9 R3). `from_bytes` / `to_bytes` are I6 dense
//! HostInvoke (Q9 R2). Allocating bodies may
//! lower to `Alloc` + `GcBarrier` SSA with live-heap `roots`; dense / LIR
//! emit across alloc only when S2b maps exist (S2c), including mapped
//! preheader `Make*` (S2d), Seek-less residuals (S2e), and S2f SROA /
//! StoreIndex-array reuse. Impure HostInvoke / CALL are SSA barriers
//! (I6); LICM hoist uses purity bits. Debugger-attached / `-Og` still
//! specialize (B8); explicit `Deopt` is skipped at emit. I8 entry is lift + cost gate after LIR
//! reconstruct walls. Q6 counted `for`, Q7 one-word rec `CALL`, and Q8
//! niche / two-slot `Br` are hygiene (lift, then cost) — not checklist
//! refuses.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]

mod abi;
mod builder;
mod call_convoy;
mod cse;
mod destprop;
mod deopt;
mod entry;
mod effects;
mod emit;
mod gc;
mod host_allow;
mod emit_lir;
mod func;
mod infer;
mod inst;
mod instcombine;
mod layout;
mod licm;
mod lower;
mod pack;
mod specialize;
mod sroa;
mod vectorize;
mod strength;
mod stackmap;
mod string_barrier;
mod text;
mod ty;

pub use abi::DenseCallMap;
pub use builder::{MirBuilder, MirError};
pub use cse::{cse, gvn};
pub use destprop::destprop;
pub use emit::emit_dense;
pub use emit_lir::emit_lir;
pub use entry::{lir_eligible, lir_refuse, LirRefuse};
pub use func::{MirBlock, MirFunc};
pub use deopt::DraftDeoptMap;
pub use stackmap::{bind_drafts, bind_heap_free_frames, try_build_draft, DraftFrameMap};
pub use inst::{
    BlockId, LocalId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirDeoptKind,
    MirGcKind, MirInst, MirUnaryOp, Terminator, ValueId,
};
pub use infer::{infer_lir_with_seed, numeric_work_ops};
pub use instcombine::instcombine;
pub use layout::MirLayout;
pub use licm::licm;
pub use lower::{LowerError, LowerHints, try_lower_numeric};
pub use specialize::{
    take_dense_refusal, take_lir_refusal, try_lower_abi_body, try_lower_abi_body_side, try_lower_abi_body_with,
    try_specialize_body, try_specialize_body_side, BodySidecar,
};
pub use sroa::sroa;
pub use strength::strength_reduce;
pub use text::{ParseError, parse_func};
pub use ty::MirTy;

/// Build the Mandelbrot inner iteration in typed SSA (acceptance for COI-267).
///
/// Corresponds to the inner `while iter < max_iter` in
/// `examples/perf/mandelbrot.hy` (`zr`/`zi`/`cr`/`ci` are `f64`, `iter` is
/// language `int` → `i64`). No classes.
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
