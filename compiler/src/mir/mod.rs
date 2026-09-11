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

pub use abi::{DenseAbi, DenseCallMap};
pub use builder::{MirBuilder, MirError};
pub use cse::{cse, gvn};
pub use destprop::destprop;
pub use emit::emit_dense;
pub use emit_lir::emit_lir;
pub use entry::{lir_eligible, lir_refuse, LirRefuse};
pub use func::{DeoptSite, MirBlock, MirFunc};
pub use gc::{fill_live_roots, LiveRootSet};
pub use deopt::{DraftDeoptMap, DraftDeoptSite};
pub use stackmap::{bind_drafts, try_build_draft, DraftFrameMap};
pub use inst::{
    BlockId, LocalId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirDeoptKind,
    MirGcKind, MirInst, MirUnaryOp, Terminator, ValueId,
};
pub use infer::{infer_lir_with_seed, infer_stack_map, numeric_work_ops};
pub use instcombine::instcombine;
pub use layout::MirLayout;
pub use licm::licm;
pub use lower::{LowerError, LowerHints, try_lower_numeric};
pub use specialize::{
    try_lower_abi_body, try_lower_abi_body_side, try_lower_abi_body_with, try_specialize_body,
    try_specialize_body_side, BodySidecar,
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
mod tests {
    use super::*;
    use crate::il::{IlJumpKind, IlOp, Label};
    use common::{Byte, DebugLoc, Instruction};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn public_api_exports_compile() {
        let _ = std::any::type_name::<(
            MirBlock,
            BlockId,
            MirCastKind,
            MirUnaryOp,
            Terminator,
            ValueId,
            LowerError,
            ParseError,
            LirRefuse,
        )>();
        let _ = (lir_eligible, lir_refuse);
    }

    #[test]
    fn pipeline_specializes_float_mul_loop() {
        let src = r#"
fn escape(float cr, float ci, int max_iter) -> int {
    let zr = 0.0;
    let zi = 0.0;
    let iter = 0;
    while iter < max_iter {
        let zr2 = zr * zr;
        let zi2 = zi * zi;
        if zr2 + zi2 > 4.0 {
            break;
        }
        let tr = zr2 - zi2 + cr;
        zi = 2.0 * zr * zi + ci;
        zr = tr;
        iter = iter + 1;
    }
    return iter;
}
fn main() {
    let _ = escape(0.0, 0.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile dense kernel");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "expected DenseBin in specialized float kernel"
        );
        assert!(
            bc.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::DenseCmp
                    | Instruction::BinSlotSlotJmpf
                    | Instruction::BinSlotSlotJmpt
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotImmJmpt
            )),
            "expected fused slot compare-jump at dense kernel edges"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_specializes_float_add_sub_without_mul_or_div() {
        let src = r#"
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf + a - b;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(2.0, 1.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile add/sub kernel");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "float +/− loops must emit DenseBin without * or /"
        );
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FADD64
            }),
            "expected DenseBin FADD64"
        );
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FSUB64
            }),
            "expected DenseBin FSUB64"
        );
        assert!(
            !bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && matches!(
                        b.dense_abc_parts().0,
                        common::dense::FMUL64 | common::dense::FDIV64
                    )
            }),
            "hit kernel must stay mul/div-free"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_specializes_nested_float_loops() {
        let src = r#"
fn nest(int n) -> float {
    let s = 0.0;
    let i = 0;
    while i < n {
        let j = 0;
        while j < n {
            s = s + (i as float) * (j as float);
            j = j + 1;
        }
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = nest(3);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile nested");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "nested float-mul loops emit dense"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_licm_hoists_invariant_divf() {
        let src = r#"
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let q = a / b;
        s = s + q * xf;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(3.0, 2.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile licm kernel");
        let fdivs = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FDIV64
            })
            .count();
        assert_eq!(fdivs, 1, "one DenseBin FDIV64 after MIR LICM");
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FMUL64
            }),
            "dense mul of the hoisted quotient"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_specializes_mandelbrot_nested() {
        let src = r#"
fn mandelbrot(int size, int max_iter) -> int {
    let sum = 0;
    let y = 0;
    while y < size {
        let x = 0;
        while x < size {
            let cr = (2.0 * (x as float) / (size as float)) - 1.5;
            let ci = (2.0 * (y as float) / (size as float)) - 1.0;
            let zr = 0.0;
            let zi = 0.0;
            let iter = 0;
            while iter < max_iter {
                let zr2 = zr * zr;
                let zi2 = zi * zi;
                if zr2 + zi2 > 4.0 {
                    break;
                }
                let tr = zr2 - zi2 + cr;
                zi = 2.0 * zr * zi + ci;
                zr = tr;
                iter = iter + 1;
            }
            sum = sum + iter;
            x = x + 1;
        }
        y = y + 1;
    }
    return sum;
}
fn main() {
    let _ = mandelbrot(8, 10);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile mandelbrot");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "flagship-shaped nested mandelbrot must emit DenseBin"
        );
        let seek = bc
            .iter()
            .filter(|b| *b.bytecode() == Instruction::Seek)
            .map(|b| b.operand_u32())
            .max()
            .unwrap_or(0);
        assert!(
            seek <= p.operand_stack_slots(),
            "dense Seek {seek} exceeds operand stack {}",
            p.operand_stack_slots()
        );
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<256>::with_operand_capacity(slots);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_cse_collapses_repeated_divf() {
        let src = r#"
fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let a = xf / scale;
        let b = xf / scale;
        s = s + a * b;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(3.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile cse kernel");
        let fdivs = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FDIV64
            })
            .count();
        assert_eq!(fdivs, 1, "MIR CSE must keep a single DenseBin FDIV64");
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FMUL64
            }),
            "dense mul of the CSE'd quotient"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_gvn_collapses_diamond_divf() {
        let src = r#"
fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        if (i & 1) == 0 {
            s = s + xf / scale;
        } else {
            s = s + xf / scale;
        }
        s = s + xf / scale;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(3.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile gvn kernel");
        let fdivs = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FDIV64
            })
            .count();
        assert_eq!(fdivs, 1, "cross-block PRE/GVN must keep a single FDIV64");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_instcombine_mul2_is_add() {
        let src = r#"
fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * scale;
        let t = xf * xf;
        s = ((s + t * 2.0) * 1.0) + 0.0;
        i = (i + 1) + 0;
    }
    return s;
}
fn main() {
    let _ = hot(2.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile instcombine kernel");
        let fmuls = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FMUL64
            })
            .count();
        let fadds = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FADD64
            })
            .count();
        assert_eq!(fmuls, 2, "scale and square; t*2.0 must become add");
        assert!(fadds >= 2, "t+t and s+…; fadds={fadds}");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_float_exact_recip_and_finite_sub() {
        let src = r#"
fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let t = xf * scale;
        s = s + t / 8.0;
        s = s + (xf - xf);
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(1.5, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile float pipeline kernel");
        let fdivs = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FDIV64
            })
            .count();
        let fsubs = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FSUB64
            })
            .count();
        let fmuls = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FMUL64
            })
            .count();
        assert_eq!(fdivs, 0, "t/8.0 must become mul by exact recip");
        assert_eq!(fsubs, 0, "cast(i)-cast(i) is known-finite +0");
        assert_eq!(fmuls, 2, "xf*scale and t*0.125; fmuls={fmuls}");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_destprop_same_value_join_cse() {
        let src = r#"
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * b;
        let t = 0.0;
        if xf > a {
            t = a + 0.0;
        } else {
            t = a * 1.0;
        }
        s = s + a * xf + t * xf + t * t;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(1.0, 2.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile destprop kernel");
        let fmuls = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FMUL64
            })
            .count();
        assert_eq!(fmuls, 3, "xf*b, CSE a*xf, and t*t→a*a; fmuls={fmuls}");
        let moves = bc
            .iter()
            .filter(|b| *b.bytecode() == Instruction::DenseMove)
            .count();
        assert!(
            moves <= 2,
            "alias join must not emit per-arm DenseMove; moves={moves}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_iv_sr_cast_times_const() {
        let src = r#"
fn hot(int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * 7.0;
        s = s + xf * xf;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile iv sr kernel");
        let fmuls = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FMUL64
            })
            .count();
        let fadds = bc
            .iter()
            .filter(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::FADD64
            })
            .count();
        assert_eq!(
            fmuls, 1,
            "cast(i)*7.0 must SR; only xf*xf remains; fmuls={fmuls}"
        );
        assert!(fadds >= 2, "induction add + acc; fadds={fadds}");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_axpy_reduce_emits_hostinvoke() {
        let src = r#"
fn pack(float a, float x0, float dx, float y, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = x0;
    while i < n {
        s = s + a * x + y;
        x = x + dx;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = pack(1.0, 0.0, 1.0, 0.0, 16);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile axpy pack");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::HostInvoke),
            "axpy-reduce must emit HostInvoke; opcodes={:?}",
            bc.iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "packed axpy must not keep DenseBin"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_vectorizes_stride1_iota_store() {
        let src = r#"
fn fill(Vec<int> v) -> int {
    let i = 0;
    while i < len(v) {
        v[i] = i;
        i = i + 1;
    }
    return len(v);
}
fn main() {
    let v: Vec<int> = Vec::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let n = fill(v);
    let s = 0;
    let i = 0;
    while i < n {
        s = s + v[i];
        i = i + 1;
    }
    if s != 120 {
        panic "fill simd checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile fill");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::VStore),
            "stride-1 iota store must emit VStore; opcodes={:?}",
            bc.iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::VBin),
            "expected VBin splat/iota"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "fill CALL must run Seek/init before V*");
        let symbols = p.program_debug().fn_symbols;
        let fill = symbols.iter().find(|s| s.name == "fill").expect("fill");
        let op = bc[fill.entry_pc as usize].bytecode();
        assert_eq!(
            *op,
            Instruction::Seek,
            "CALL target must be the V* prelude, not the loop header ({})",
            op.mnemonic()
        );
    }

    #[test]
    fn pipeline_vectorizes_stride1_reduce() {
        let src = r#"
fn scan(Vec<int> v) -> int {
    let acc = 0;
    let i = 0;
    while i < len(v) {
        acc = acc + v[i];
        i = i + 1;
    }
    return acc;
}
fn main() {
    let v: Vec<int> = Vec::from([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    if scan(v) != 120 {
        panic "scan simd checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile scan");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::VReduce),
            "stride-1 add-reduce must emit VReduce; opcodes={:?}",
            bc.iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "scan reduce checksum");
    }

    #[test]
    fn pipeline_vectorizes_conservative_fma_store() {
        let src = r#"
fn axpy(float a, Vec<float> x, Vec<float> y) -> float {
    let i = 0;
    while i < len(x) {
        y[i] = a * x[i] + y[i];
        i = i + 1;
    }
    return y[0];
}
fn main() {
    let x: Vec<float> = Vec::from([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0]);
    let y: Vec<float> = Vec::from([1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    let z = axpy(2.0, x, y);
    if z != 3.0 {
        panic "axpy simd checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile axpy");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::VFma),
            "saxpy store must emit VFma; opcodes={:?}",
            bc.iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "axpy FMA checksum");
    }

    #[test]
    fn pipeline_specializes_counted_i64_loop() {
        let src = r#"
fn sum(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = sum(10);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile i64 loop");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "counted i64 loops must emit DenseBin"
        );
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::IADD64
            }),
            "expected DenseBin IADD64"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_specializes_i64_add_sub_without_mul_or_div() {
        let src = r#"
fn hot(int a, int b, int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + i + a - b;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(2, 1, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile i64 add/sub");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "counted i64 +/− loops must emit DenseBin"
        );
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::IADD64
            }),
            "expected DenseBin IADD64"
        );
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && b.dense_abc_parts().0 == common::dense::ISUB64
            }),
            "expected DenseBin ISUB64"
        );
        assert!(
            !bc.iter().any(|b| {
                *b.bytecode() == Instruction::DenseBin
                    && matches!(
                        b.dense_abc_parts().0,
                        common::dense::IMUL64 | common::dense::IDIV64
                    )
            }),
            "W2 hit kernel must stay mul/div-free"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_refuses_straight_line_when_cost_loses() {
        let src = r#"
fn eval_a(int i, int j) -> int {
    return i + j * 2;
}
fn main() {
    let _ = eval_a(1, 2);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile below-gate i64");
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "tiny straight-line helper loses the dense cost gate vs fuse-IL"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_i8_compare_diamond_stays_not_dense() {
        let src = r#"
fn pick(int a, int b, bool c) -> int {
    if c {
        return a;
    }
    return b;
}
fn main() {
    let _ = pick(1, 2, true) + pick(3, 4, false);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile I8 pick");
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "compare diamond must not dense-specialize"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_specializes_straight_line_above_work_gate() {
        let src = r#"
fn hot(float x, float y) -> float {
    let a = x * x + y * y;
    let b = a * x + y * 2.0;
    let c = b * y - x * 4.0;
    let d = c * x + a * 0.5;
    return d / (2.0 + x * y);
}
fn main() {
    let _ = hot(1.5, 0.5);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile above-gate kernel");
        assert_eq!(numeric_work_ops(&[]), 0);
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "straight-line kernel that beats fuse-IL cost must emit DenseBin"
        );
        assert!(
            !bc.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::BinSlotSlotJmpf
                    | Instruction::BinSlotSlotJmpt
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotImmJmpt
            )),
            "W3 kernel is acyclic; no fused compare-jump latch"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_specializes_allowlisted_math_host_inside_dense() {
        let src = r#"
fn hot(float a, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + sin(xf) * a;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(2.0, 4);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile W4 math host");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "allowlisted math HostInvoke must stay on dense; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::HostInvoke),
            "W4 edge must emit HostInvoke inside the dense body"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_specializes_dense_to_dense_call() {
        let src = r#"
fn kernel(float x, int k) -> float {
    let i = 0;
    let t = x;
    while i < k {
        t = t * 0.5 + x;
        i = i + 1;
    }
    return t;
}
fn hot(float a, float dx, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = 0.125;
    while i < n {
        s = s + kernel(x, 8) * a + dx;
        x = x + dx;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(1.0, 0.0, 2);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile dense CALL");
        let hot = p.function_offset("hot").expect("hot");
        let main = p.function_offset("main").expect("main");
        let hot_bc = if hot < main { &bc[hot..main] } else { &bc[hot..] };
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "caller must stay dense; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::CALL),
            "dense caller must emit CALL to the dense leaf"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_open_call_to_fuse_il_helper_is_dense() {
        let src = r#"
fn mid(float x) -> float {
    let a = x * x + x;
    let b = a * x + x;
    let c = b * x + a;
    return c + 1.0;
}
fn hot(float a, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        s = s + mid(a) + a;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(1.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile mid CALL");
        let hot = p.function_offset("hot").expect("hot");
        let main = p.function_offset("main").expect("main");
        let hot_bc = if hot < main { &bc[hot..main] } else { &bc[hot..] };
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S3 open one-word CALL keeps the caller dense; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::CALL),
            "dense caller still emits CALL"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_open_call_to_recursive_helper_is_dense() {
        let src = r#"
fn helper(float x, int k) -> float {
    if k <= 0 {
        return x * 2.0;
    }
    return helper(x, k - 1);
}
fn hot(float a, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        s = s + helper(a, i) + a;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(1.0, 8);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile open CALL");
        let hot = p.function_offset("hot").expect("hot");
        let main = p.function_offset("main").expect("main");
        let hot_bc = if hot < main { &bc[hot..main] } else { &bc[hot..] };
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S3 open CALL densifies the loop; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::CALL),
            "recursive helper stays a direct CALL"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_recursive_work_loop_is_dense() {
        // Tight tak/fib leafs convoy CALL on the operand stack (B2) so they
        // can win the cost gate without a prologue Seek. A self-recursive
        // body with a counted loop still densifies + typed CALL (Q7).
        let src = r#"
fn rec(int n) -> int {
    if n <= 0 {
        return 1;
    }
    let i = 0;
    let s = 0;
    while i < 8 {
        s = s + n * i;
        i = i + 1;
    }
    return rec(n - 1) + s;
}
fn main() {
    let _ = rec(6);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile rec");
        let rec = p.function_offset("rec").expect("rec");
        let main = p.function_offset("main").expect("main");
        let rec_bc = if rec < main { &bc[rec..main] } else { &bc[rec..] };
        assert!(
            rec_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "Q7 recursive+loop callee must densify; opcodes={:?}",
            rec_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            rec_bc.iter().any(|b| *b.bytecode() == Instruction::CALL),
            "typed recursive CALL must remain"
        );
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<256>::with_operand_capacity(slots);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_fib_leaf_convoys_call_without_seek() {
        let src = r#"
fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let _ = fib(10);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile fib");
        let fib = p.function_offset("fib").expect("fib");
        let main = p.function_offset("main").expect("main");
        let fib_bc = if fib < main { &bc[fib..main] } else { &bc[fib..] };
        assert!(
            !fib_bc.iter().any(|b| *b.bytecode() == Instruction::Seek),
            "B2: param-only fib must not Seek; opcodes={:?}",
            fib_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let call_pos: Vec<usize> = fib_bc
            .iter()
            .enumerate()
            .filter(|(_, b)| *b.bytecode() == Instruction::CALL)
            .map(|(i, _)| i)
            .collect();
        assert!(
            call_pos.len() >= 2,
            "fib must keep two recursive CALLs; opcodes={:?}",
            fib_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let no_store_between = call_pos.windows(2).any(|w| {
            !(w[0] + 1..w[1]).any(|i| {
                matches!(
                    *fib_bc[i].bytecode(),
                    Instruction::STORE | Instruction::StorePop
                )
            })
        });
        assert!(
            no_store_between,
            "B2: recursive CALL results stay on the stack; opcodes={:?}",
            fib_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "fib(10) must run");
    }

    #[test]
    fn pipeline_sibling_tail_even_odd_stays_correct() {
        let src = r#"
fn even(int n) -> int {
    if n == 0 {
        return 1;
    }
    return odd(n - 1);
}
fn odd(int n) -> int {
    if n == 0 {
        return 0;
    }
    return even(n - 1);
}
fn main() {
    if even(10) != 1 {
        panic("even10");
    }
    if odd(10) != 0 {
        panic("odd10");
    }
    if even(1) != 0 {
        panic("even1");
    }
    if odd(1) != 1 {
        panic("odd1");
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile sibling tail");
        let even = p.function_offset("even").expect("even");
        let odd = p.function_offset("odd").expect("odd");
        let main = p.function_offset("main").expect("main");
        let even_end = odd.min(main);
        let even_bc = if even < even_end { &bc[even..even_end] } else { &bc[even..] };
        let even_ops: Vec<_> = even_bc
            .iter()
            .map(|b| format!("{}:{}", b.bytecode().mnemonic(), b.operand_u32()))
            .collect();
        let odd_end = main;
        let odd_bc = if odd < odd_end { &bc[odd..odd_end] } else { &bc[odd..] };
        let odd_ops: Vec<_> = odd_bc
            .iter()
            .map(|b| format!("{}:{}", b.bytecode().mnemonic(), b.operand_u32()))
            .collect();
        assert!(
            even_bc.iter().any(|b| *b.bytecode() == Instruction::TailCall),
            "sibling even/odd must keep TailCall; opcodes={even_ops:?}"
        );
        assert!(
            !even_bc.iter().any(|b| *b.bytecode() == Instruction::Seek),
            "B7 TailCall stack-arg must not tax a param-only leaf with Seek; opcodes={even_ops:?}"
        );
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<64>::with_operand_capacity(slots.max(64));
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(
            !vm.panicked(),
            "even/odd sibling TailCall must be correct; even={even_ops:?} odd={odd_ops:?}"
        );
    }

    #[test]
    fn pipeline_two_slot_match_call_loop_is_dense_or_lir() {
        let src = r#"
fn lookup(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}
fn hot(int n, int iters) -> int {
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match lookup(i % 10, n) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        i = i + 1;
    }
    return acc;
}
fn main() {
    let _ = hot(7, 40);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile two-slot match+call");
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::CALL && b.call_ret_words() >= 2),
            "two-slot CALL must remain; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::MakeEnum),
            "match+call must not box; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let hot = p.function_offset("hot").expect("hot");
        let main = p.function_offset("main").expect("main");
        let hot_bc = if hot < main { &bc[hot..main] } else { &bc[hot..] };
        let lifted = hot_bc.iter().any(|b| {
            matches!(
                *b.bytecode(),
                Instruction::DenseBin | Instruction::DenseConst | Instruction::DensePush
            )
        });
        let _ = lifted;
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<64>::with_operand_capacity(slots.max(64));
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "hot(7, 40) must run");
    }

    #[test]
    fn pipeline_two_word_sibling_tail_stays_fuse() {
        let src = r#"
fn bounce_a(Option<int> o) -> Option<int> {
    return bounce_b(o);
}
fn bounce_b(Option<int> o) -> Option<int> {
    return match o {
        Option::None => Option::Some(1),
        Option::Some(x) => bounce_a(Option::None),
    };
}
fn main() {
    let r = bounce_a(Option::None);
    let v = match r {
        Option::Some(x) => x,
        Option::None => 0,
    };
    if v != 1 {
        panic("bounce");
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile two-word sibling");
        let a = p.function_offset("bounce_a").expect("bounce_a");
        let b = p.function_offset("bounce_b").expect("bounce_b");
        let main = p.function_offset("main").expect("main");
        let a_end = b.min(main);
        let a_bc = if a < a_end { &bc[a..a_end] } else { &bc[a..] };
        let a_ops: Vec<_> = a_bc.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            a_bc.iter().any(|b| *b.bytecode() == Instruction::TailCall),
            "two-slot sibling must keep TailCall; opcodes={a_ops:?}"
        );
        assert!(
            !a_bc.iter().any(|b| *b.bytecode() == Instruction::DensePush),
            "B7 two-slot TailCall must not DensePush args; opcodes={a_ops:?}"
        );
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<64>::with_operand_capacity(slots.max(64));
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "bounce_a must return Some(1); opcodes={a_ops:?}");
    }

    #[test]
    fn pipeline_mutual_call_and_self_two_slot_stay_correct() {
        let src = r#"
#[max_depth(16)]
fn ping(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return 1 + pong(n - 1);
}
#[max_depth(16)]
fn pong(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return 1 + ping(n - 1);
}
fn main() {
    if ping(4) != 4 {
        panic("ping");
    }
    if pong(3) != 3 {
        panic("pong");
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile B7 mutual CALL");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::CALL)
                || bc.iter().any(|b| *b.bytecode() == Instruction::TailCall),
            "mutual / self two-slot must keep CALL or TailCall; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<64>::with_operand_capacity(slots.max(64));
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "ping/pong must run");
    }

    #[test]
    fn pipeline_self_two_slot_call_return_checksum() {
        let src = r#"
#[max_depth(32)]
fn walk(int n) -> Option<int> {
    if n <= 0 {
        return Option::Some(0);
    }
    let r = walk(n - 1);
    let x = match r {
        Option::Some(v) => v,
        Option::None => 0,
    };
    return Option::Some(x + 1);
}
#[max_depth(32)]
fn walk_tail(int n, int acc) -> Option<int> {
    if n <= 0 {
        return Option::Some(acc);
    }
    return walk_tail(n - 1, acc + 1);
}
fn main() {
    let a = match walk(6) {
        Option::Some(x) => x,
        Option::None => -1,
    };
    let b = match walk_tail(6, 0) {
        Option::Some(x) => x,
        Option::None => -1,
    };
    if a != 6 {
        panic("walk");
    }
    if b != 6 {
        panic("walk_tail");
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile C1 self two-slot");
        assert!(
            bc.iter().any(|b| {
                (*b.bytecode() == Instruction::CALL && b.call_ret_words() >= 2)
                    || *b.bytecode() == Instruction::TailCall
            }),
            "self two-slot must keep CALL/TailCall; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::MakeEnum),
            "self two-slot must not box; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let walk = p.function_offset("walk").expect("walk");
        let walk_tail = p.function_offset("walk_tail").expect("walk_tail");
        let main = p.function_offset("main").expect("main");
        let slice = |start: usize| {
            let end = [walk, walk_tail, main, bc.len()]
                .into_iter()
                .filter(|&o| o > start)
                .min()
                .unwrap_or(bc.len());
            &bc[start..end]
        };
        let walk_bc = slice(walk);
        let walk_ops: Vec<_> = walk_bc.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            walk_bc
                .iter()
                .any(|b| *b.bytecode() == Instruction::CALL && b.call_ret_words() >= 2),
            "walk must keep self two-slot CALL; opcodes={walk_ops:?}"
        );
        let tail_bc = slice(walk_tail);
        let tail_ops: Vec<_> = tail_bc.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            tail_bc.iter().any(|b| *b.bytecode() == Instruction::TailCall),
            "walk_tail must keep TailCall; opcodes={tail_ops:?}"
        );
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<64>::with_operand_capacity(slots.max(64));
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "walk/walk_tail checksum");
    }

    #[test]
    fn pipeline_tail_sibling_hy_harness_cases_pass() {
        let src = include_str!("../../../tests/positive/tail_sibling.hy");
        let mut p = crate::Pipeline::new();
        p.set_include_tests(true);
        let (bc, constants) = p.compile_src(src).expect("compile tail_sibling.hy");
        let cases = p.test_cases().to_vec();
        assert_eq!(
            cases.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["sibling tail even/odd", "two-word sibling tail"]
        );
        for (name, offset) in &cases {
            let slots = p.operand_stack_slots() as usize;
            let mut vm = machine::Machine::<64>::with_operand_capacity(slots.max(64));
            p.wire_host_natives(&mut vm);
            vm.load_program(&bc, &constants, p.strings());
            let ret = vm.call_function(*offset, &[]);
            assert!(
                !vm.panicked() && vm.result_is_ok(ret),
                "{name} must pass"
            );
        }
    }

    #[test]
    fn pipeline_eval_a_follows_straight_line_work_gate() {
        let src = r#"
fn eval_a(int i, int j) -> float {
    let ij = i + j;
    let t = (ij * (ij + 1)) / 2 + i + 1;
    return 1.0 / (t as float);
}
fn main() {
    let _ = eval_a(1, 2);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, _) = p.compile_src(src).expect("compile eval_a");
        let dense = bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin);
        assert!(
            dense,
            "nbody eval_a beats the dense cost gate and emits DenseBin"
        );
    }

    #[test]
    fn mandelbrot_inner_is_typed_ssa() {
        let f = mandelbrot_inner_loop().expect("mandel inner");
        f.verify().unwrap();
        assert_eq!(f.params.len(), 5);
        assert!(f.params.iter().any(|&p| f.ty(p) == MirTy::F64));
        assert!(f.params.iter().any(|&p| f.ty(p) == MirTy::I64));
        let header = f
            .blocks
            .iter()
            .find(|b| {
                b.insts
                    .iter()
                    .any(|i| matches!(i, MirInst::Phi { ty: MirTy::F64, .. }))
                    && b.insts
                        .iter()
                        .any(|i| matches!(i, MirInst::Phi { ty: MirTy::I64, .. }))
            })
            .expect("header phis for zr/zi/iter");
        assert!(
            header
                .insts
                .iter()
                .filter(|i| matches!(i, MirInst::Phi { ty: MirTy::F64, .. }))
                .count()
                >= 2
        );
        let text = f.to_string();
        assert!(text.contains("fmul"), "{text}");
        assert!(text.contains("fcmp.ogt") || text.contains("fcmp"), "{text}");
        let g = parse_func(&text).expect(&text);
        g.verify().unwrap();
        assert_eq!(parse_func(&g.to_string()).unwrap(), g);
    }

    #[test]
    fn lowering_mandelbrot_like_il() {
        // zr,zi,cr,ci in slots 0..3 (f64); iter slot 4; max slot 5 (i64).
        let four = 0u32;
        let two = 1u32;
        let ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 6,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 7,
                loc: loc(),
            },
            IlOp::Load {
                slot: 6,
                loc: loc(),
            },
            IlOp::Load {
                slot: 7,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::ConstPool {
                idx: four,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::GTF,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 6,
                loc: loc(),
            },
            IlOp::Load {
                slot: 7,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 8,
                loc: loc(),
            },
            IlOp::ConstPool {
                idx: two,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 8,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(2)),
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        let mut hints = LowerHints::new("mandel_il");
        for s in 0..4 {
            hints.slot_ty.insert(s, MirTy::F64);
        }
        hints.slot_ty.insert(4, MirTy::I64);
        hints.slot_ty.insert(5, MirTy::I64);
        hints.slot_ty.insert(6, MirTy::F64);
        hints.slot_ty.insert(7, MirTy::F64);
        hints.slot_ty.insert(8, MirTy::F64);
        hints.pool = vec![4.0f64.to_bits(), 2.0f64.to_bits()];
        hints.pool_ty = vec![Some(MirTy::F64), Some(MirTy::F64)];
        let f = try_lower_numeric(&ops, &hints).expect("lower mandel-like IL");
        f.verify().unwrap();
        let mut pool = hints.pool.clone();
        let dense = emit_dense(&f, Some(Label(0)), &mut pool, false).expect("emit");
        assert!(
            dense.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseBin
            )),
            "dense emit must use DenseBin"
        );
        assert!(f.blocks.iter().any(|bl| {
            bl.insts.iter().any(|i| {
                matches!(
                    i,
                    MirInst::Bin {
                        op: MirBinOp::Mul,
                        ty: MirTy::F64,
                        ..
                    }
                )
            })
        }));
    }

    fn two_slot_il(loc: DebugLoc) -> Vec<IlOp> {
        vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 1, loc },
            IlOp::Const { imm: 0, loc },
            IlOp::Bin {
                op: Instruction::EQ,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Const { imm: -1, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Return { loc, ret_words: 2 },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: Instruction::DIV,
                loc,
            },
            IlOp::Const { imm: 0, loc },
            IlOp::Return { loc, ret_words: 2 },
        ]
    }

    #[test]
    fn two_slot_result_lowers_to_lir() {
        let loc = loc();
        let ops = two_slot_il(loc);
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "checked_div", 2, &mut pool).expect("abi leaf");
        assert!(
            lir.iter()
                .any(|op| matches!(op, IlOp::Return { ret_words: 2, .. })),
            "LIR must keep two-slot RETURN"
        );
        assert!(
            !lir.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if matches!(
                    *byte.bytecode(),
                    Instruction::DenseBin | Instruction::MakeEnum
                )
            )),
            "P3 must not emit dense or MakeEnum"
        );
        let mut hints = LowerHints::new("checked_div");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.slot_ty.insert(1, MirTy::I64);
        hints.param_count = 2;
        let f = try_lower_numeric(&ops, &hints).expect("lower ret2");
        f.verify().unwrap();
        assert_eq!(f.ret_layout, MirLayout::TwoSlot);
        assert!(
            f.blocks.iter().any(|b| matches!(
                b.term,
                Some(Terminator::Return {
                    lo: Some(_),
                    hi: Some(_),
                })
            )),
            "SSA return is a pair"
        );
        let text = f.to_string();
        assert!(text.contains("return "), "{text}");
        let g = parse_func(&text).expect(&text);
        g.verify().unwrap();
        assert_eq!(g.ret_layout, MirLayout::TwoSlot);
        let _ = emit_dense(&f, Some(Label(0)), &mut pool, false);
        assert!(
            !lir.iter().any(|op| matches!(op, IlOp::StorePop { .. })),
            "return/cmp immediates must stay on the stack"
        );
        assert!(
            !lir.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek
            )),
            "leaf using only param slots must not Seek"
        );
    }

    #[test]
    fn pair_lir_keeps_shared_rem_in_a_slot() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 10, loc },
            IlOp::Bin {
                op: Instruction::MOD,
                loc,
            },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Dup { loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 2 },
        ];
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "pair", 1, &mut pool).expect("pair leaf");
        let mods = lir
            .iter()
            .filter(|op| {
                matches!(op, IlOp::Bin { op, .. } if *op == Instruction::MOD)
                    || matches!(
                        op,
                        IlOp::BinSlotSlot { op, .. } | IlOp::BinSlotImm { op, .. }
                            if common::Instruction::from(*op) == Instruction::MOD
                    )
            })
            .count();
        assert_eq!(
            mods, 1,
            "shared k = i % 10 must be stored, not rematerialized"
        );
        assert!(
            lir.iter().any(|op| matches!(
                op,
                IlOp::BinSlotImm { op, imm: 10, .. }
                    if common::Instruction::from(*op) == Instruction::MOD
            )),
            "i % 10 should be BinSlotImm so replace cost matches opted IL"
        );
        assert!(
            lir.iter().any(|op| matches!(op, IlOp::StorePop { .. })),
            "shared rem needs a slot"
        );
        assert!(
            lir.iter().any(|op| matches!(op, IlOp::Dup { .. })),
            "return (k, k+1) should DUP TOS"
        );
    }

    #[test]
    fn i1_lower_carries_heap_and_niche_words() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        for ty in [MirTy::HeapRef, MirTy::NicheOpt, MirTy::NicheRes] {
            let mut hints = LowerHints::new("carry");
            hints.slot_ty.insert(0, ty);
            hints.param_count = 1;
            let f = try_lower_numeric(&ops, &hints).expect("lower heap/niche copy");
            f.verify().unwrap();
            assert_eq!(f.ty(f.params[0]), ty);
            assert_eq!(f.ret_ty, Some(ty));
            assert_eq!(f.ret_layout, ty.layout());
            let mut pool = Vec::new();
            assert!(
                emit_dense(&f, Some(Label(0)), &mut pool, false).is_ok(),
                "Q8: niche / HeapRef are dense word lanes"
            );
        }
    }

    #[test]
    fn i1_infer_seed_carries_bitor_niche_res() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::BITOR,
                loc,
            },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut seed = std::collections::HashMap::new();
        seed.insert(0, MirTy::HeapRef);
        let inferred = infer_lir_with_seed(&ops, 0, 1, &seed).expect("infer seed");
        assert_eq!(inferred.slot_ty.get(&0), Some(&MirTy::HeapRef));
        assert_eq!(inferred.slot_ty.get(&1), Some(&MirTy::NicheRes));
        assert!(
            super::infer::infer_numeric(&ops, 0, 1).is_err(),
            "dense infer still refuses this body"
        );
    }

    #[test]
    fn niche_word_is_one_value_lir() {
        // Err = ptr | 1; one-word heap-heap Result (no pair opcode).
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::BITOR,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut hints = LowerHints::new("niche_err");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.param_count = 1;
        let f = try_lower_numeric(&ops, &hints).expect("lower niche word");
        f.verify().unwrap();
        assert_eq!(f.ret_layout, MirLayout::Word);
        let mut pool = Vec::new();
        let lir = emit_lir(&f, Some(Label(0)), &mut pool, false).expect("emit niche");
        assert!(
            lir.iter()
                .any(|op| matches!(op, IlOp::Return { ret_words: 1, .. }))
        );
        assert!(
            lir.iter().any(|op| matches!(
                op,
                IlOp::BinSlotSlot { op, .. } | IlOp::BinSlotImm { op, .. }
                    if common::Instruction::from(*op) == Instruction::BITOR
            ) || matches!(op, IlOp::Bin { op, .. } if *op == Instruction::BITOR)),
            "niche LIR must emit BITOR"
        );
    }

    #[test]
    fn i8_one_word_niche_enters_lir_without_match() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::BITOR,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert!(lir_eligible(&ops, &[]));
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "niche_err", 1, &mut pool)
            .expect("I8 one-word niche is LIR, not a match accident");
        assert!(
            lir.iter()
                .any(|op| matches!(op, IlOp::Return { ret_words: 1, .. }))
        );
        assert!(
            try_specialize_body(&ops, "niche_err", 1, &mut pool, &DenseCallMap::new(), None)
                .is_none()
        );
    }

    #[test]
    fn i8_plain_if_diamond_enters_lir() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: Instruction::LE,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert!(lir_eligible(&ops, &[]));
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "min", 2, &mut pool).expect("plain if diamond is LIR");
        assert!(lir.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn i2_niche_option_match_enters_lir() {
        let loc = loc();
        // match opt { Some(x) => x, None => 0 } on a pointer-niche word.
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Dup { loc },
            IlOp::LogNot { loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Pop { loc },
            IlOp::Const { imm: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "niche_match", 1, &mut pool).expect("I2 niche leaf");
        assert!(
            lir.iter()
                .any(|op| matches!(op, IlOp::Return { ret_words: 1, .. }))
        );
        assert!(
            !lir.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if matches!(
                    *byte.bytecode(),
                    Instruction::DenseBin | Instruction::MakeEnum
                )
            )),
            "I2 must stay MIR→LIR"
        );
        let mut hints = LowerHints::new("niche_match");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.param_count = 1;
        hints.allow_match = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower niche match");
        f.verify().unwrap();
        assert!(
            f.blocks.iter().any(|b| matches!(b.term, Some(Terminator::Br { .. }))),
            "niche sentinel is Br (LogNot)"
        );
        let dense = emit_dense(&f, Some(Label(0)), &mut pool, false).expect("Q8 niche Br");
        assert!(
            !dense.iter().any(|op| matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfMatch { .. },
                    ..
                }
            )),
            "Q8 reconstruct must not re-emit stack JumpIfMatch"
        );
    }

    #[test]
    fn i2_jump_if_match_lowers_and_emits() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Pop { loc },
            IlOp::Const { imm: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "jim", 1, &mut pool).expect("I2 JumpIfMatch leaf");
        assert!(
            lir.iter().any(|op| matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
                    ..
                }
            )),
            "LIR must emit JumpIfMatch"
        );
        let mut hints = LowerHints::new("jim");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.param_count = 1;
        hints.allow_match = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower jim");
        f.verify().unwrap();
        assert!(
            f.blocks.iter().any(|b| matches!(
                b.term,
                Some(Terminator::JumpIfMatch { tag: 1, .. })
            )),
            "SSA terminator is JumpIfMatch"
        );
        let text = f.to_string();
        assert!(text.contains("jumpifmatch"), "{text}");
        let g = parse_func(&text).expect(&text);
        g.verify().unwrap();
        assert!(emit_dense(&f, Some(Label(0)), &mut pool, false).is_err());
    }

    #[test]
    fn i2_pair_match_one_word_return_enters_lir() {
        let loc = loc();
        // Two-slot [payload, tag] match: Ok(x) => x, Err(e) => e
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Dup { loc },
            IlOp::Const { imm: 0, loc },
            IlOp::Bin {
                op: Instruction::EQ,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Pop { loc },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Pop { loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "pair_match", 2, &mut pool)
            .expect("I2 two-slot match leaf");
        assert!(
            lir.iter()
                .any(|op| matches!(op, IlOp::Return { ret_words: 1, .. }))
        );
        let mut hints = LowerHints::new("pair_match");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.slot_ty.insert(1, MirTy::I64);
        hints.param_count = 2;
        hints.allow_match = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower two-slot match");
        f.verify().unwrap();
        assert!(
            f.blocks
                .iter()
                .any(|b| matches!(b.term, Some(Terminator::Br { .. }))),
            "two-slot tag match is Br"
        );
        let dense = emit_dense(&f, Some(Label(0)), &mut pool, false)
            .expect("Q8 two-slot tag Br is dense");
        assert!(
            !dense.iter().any(|op| matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfMatch { .. },
                    ..
                }
            )),
            "Q8 two-slot reconstruct is register Br, not JumpIfMatch"
        );
    }

    #[test]
    fn q8_two_slot_match_plus_arith_emits_dense() {
        let loc = loc();
        // Two-slot [payload, tag]: Ok(x) => x + 1, Err(_) => 0
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Dup { loc },
            IlOp::Const { imm: 0, loc },
            IlOp::Bin {
                op: Instruction::EQ,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Pop { loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Pop { loc },
            IlOp::Pop { loc },
            IlOp::Const { imm: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        let mut hints = LowerHints::new("pair_add");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.slot_ty.insert(1, MirTy::I64);
        hints.param_count = 2;
        hints.allow_match = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower Q8 two-slot+arith");
        f.verify().unwrap();
        let dense = emit_dense(&f, Some(Label(0)), &mut pool, false).expect("Q8 emit");
        assert!(
            dense.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseBin
            )),
            "Q8 two-slot+arith uses DenseBin"
        );
        assert!(
            !dense.iter().any(|op| matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfMatch { .. },
                    ..
                }
            )),
            "no stack JumpIfMatch"
        );
        let _ =
            try_specialize_body(&ops, "pair_add", 2, &mut pool, &DenseCallMap::new(), None);
    }

    #[test]
    fn i3_unboxed_field_load_store_lowers() {
        let loc = loc();
        // let p = new Point(5, 6); return p.x + p.y
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 5, loc },
            IlOp::StorePop { slot: 0, loc },
            IlOp::Const { imm: 6, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        let lir = try_lower_abi_body_with(&ops, "point_sum", 0, &mut pool, &[(0, 2)])
            .expect("I3 unboxed field leaf");
        assert!(
            lir.iter()
                .any(|op| matches!(op, IlOp::Return { ret_words: 1, .. }))
        );
        assert!(
            !lir.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if matches!(
                    *byte.bytecode(),
                    Instruction::DenseBin | Instruction::InitTyped | Instruction::GetField
                )
            )),
            "I3 must stay MIR→LIR without heap fields"
        );
        let mut hints = LowerHints::new("point_sum");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.slot_ty.insert(1, MirTy::I64);
        hints.allow_fields = true;
        hints.unboxed_fields = vec![(0, 2)];
        let f = try_lower_numeric(&ops, &hints).expect("lower unboxed fields");
        f.verify().unwrap();
        assert!(
            f.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|i| matches!(i, MirInst::FieldLoad { index: 0, .. }))
            }),
            "SSA has FieldLoad"
        );
        assert!(
            f.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|i| matches!(i, MirInst::FieldStore { index: 1, .. }))
            }),
            "SSA has FieldStore"
        );
        let text = f.to_string();
        assert!(text.contains("fieldload"), "{text}");
        assert!(text.contains("fieldstore"), "{text}");
        let g = parse_func(&text).expect(&text);
        g.verify().unwrap();
        assert!(emit_dense(&f, Some(Label(0)), &mut pool, false).is_err());
    }

    #[test]
    fn i5_alloc_edges_visible_and_emit_refuses() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::Const { imm: 2, loc },
            IlOp::MakeArray { arity: 2, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        assert!(
            try_lower_abi_body(&ops, "arr", 0, &mut pool).is_some(),
            "S2c mapped MakeArray leaf may take LIR"
        );
        assert!(
            super::infer::infer_numeric(&ops, 0, 0).is_err(),
            "I5 dense infer refuses MakeArray"
        );
        let mut hints = LowerHints::new("arr");
        hints.allow_alloc = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower alloc");
        f.verify().unwrap();
        assert!(f.has_gc_edge());
        assert!(
            f.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::Array, .. }))
            }),
            "Alloc.array visible"
        );
        assert!(
            f.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|i| matches!(i, MirInst::GcBarrier { kind: MirGcKind::Safepoint, .. }))
            }),
            "GcBarrier safepoint visible"
        );
        let obj = f.blocks.iter().find_map(|b| {
            b.insts.iter().find_map(|i| match i {
                MirInst::Alloc { dest, .. } => Some(*dest),
                _ => None,
            })
        });
        let roots = f.blocks.iter().find_map(|b| {
            b.insts.iter().find_map(|i| match i {
                MirInst::GcBarrier { roots, .. } => Some(roots.clone()),
                _ => None,
            })
        });
        let obj = obj.expect("alloc dest");
        let roots = roots.expect("barrier roots");
        assert!(
            roots.contains(&obj),
            "S2a roots include new object: {roots:?}"
        );
        let text = f.to_string();
        assert!(text.contains("alloc.array"), "{text}");
        assert!(text.contains("gcbarrier.safepoint"), "{text}");
        let g = parse_func(&text).expect(&text);
        g.verify().unwrap();
        assert!(g.has_gc_edge());
        assert!(emit_dense(&f, Some(Label(0)), &mut pool, false).is_err());
        assert!(emit_lir(&f, Some(Label(0)), &mut pool, false).is_err());
        let lir = emit_lir(&f, Some(Label(0)), &mut pool, true).expect("S2c mapped LIR");
        assert!(
            lir.iter().any(|op| matches!(op, IlOp::MakeArray { .. })),
            "mapped emit reconstructs MakeArray"
        );
    }

    #[test]
    fn b6_array_push_lowers_and_maps() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::byte(Byte::new(Instruction::ArrayPush)),
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut hints = LowerHints::new("grow");
        hints.allow_alloc = true;
        hints.slot_ty.insert(0, MirTy::HeapRef);
        let f = try_lower_numeric(&ops, &hints).expect("lower ArrayPush");
        f.verify().unwrap();
        assert!(f.has_gc_edge());
        assert!(f.blocks.iter().any(|b| {
            b.insts
                .iter()
                .any(|i| matches!(i, MirInst::ArrayPush { .. }))
        }));
        let draft = super::stackmap::try_build_draft(&ops, "grow", 1, &[], &[]).expect("maps");
        assert_eq!(draft.sites.len(), 1);
        let mut pool = Vec::new();
        assert!(
            try_lower_abi_body(&ops, "grow", 1, &mut pool).is_some(),
            "mapped ArrayPush may take LIR"
        );
    }

    #[test]
    fn b6_nsieve_binds_grow_maps() {
        let src = r#"
fn fill(int n) -> int {
    let xs: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        xs.push(i);
        i = i + 1;
    }
    return xs[n - 1];
}
fn main() {
    if fill(4) != 3 {
        panic "fill";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let _ = p.compile_src(src).expect("compile fill");
        assert!(
            p.stack_maps()
                .iter()
                .any(|m| !m.safepoints.is_empty()),
            "B6 Vec.push should bind grow maps: {:?}",
            p.stack_maps()
        );
    }

    #[test]
    fn b6_call_plus_makearray_binds_maps() {
        let src = r#"
fn leaf(int n) -> int {
    return n + 1;
}
fn wrap(int n) -> [int] {
    return [leaf(n), n];
}
fn main() {
    let xs = wrap(2);
    if xs[0] + xs[1] != 5 {
        panic "wrap";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let _ = p.compile_src(src).expect("compile wrap");
        assert!(
            p.stack_maps()
                .iter()
                .any(|m| !m.safepoints.is_empty()),
            "B6 CALL+MakeArray should bind maps: {:?}",
            p.stack_maps()
        );
    }

    #[test]
    fn b6_recursive_make_enum_binds_maps() {
        let src = r#"
enum Tree {
    Leaf,
    Node(Tree, Tree),
}
#[max_depth(8)]
fn bottom_up(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(bottom_up(depth - 1), bottom_up(depth - 1));
}
fn main() {
    let _ = bottom_up(2);
}
"#;
        let mut p = crate::Pipeline::new();
        let _ = p.compile_src(src).expect("compile bottom_up");
        assert!(
            p.stack_maps()
                .iter()
                .any(|m| !m.safepoints.is_empty()),
            "B6 recursive MakeEnum should bind maps: {:?}",
            p.stack_maps()
        );
    }

    #[test]
    fn i5_init_typed_lowers_object_alloc() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Byte {
                byte: Byte::new(Instruction::InitTyped)
                    .with_operand_u32(common::pack_init_typed(3, 2)),
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut hints = LowerHints::new("obj");
        hints.allow_alloc = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower InitTyped");
        f.verify().unwrap();
        assert!(f.blocks.iter().any(|b| {
            b.insts.iter().any(|i| {
                matches!(
                    i,
                    MirInst::Alloc {
                        kind: MirAllocKind::Object {
                            type_id: 3,
                            nfields: 2
                        },
                        ..
                    }
                )
            })
        }));
        let mut pool = Vec::new();
        assert!(
            try_lower_abi_body(&ops, "obj", 0, &mut pool).is_some(),
            "S2c mapped InitTyped may take LIR"
        );
    }

    #[test]
    fn pipeline_inlined_take_makearray_stays_fuse_il() {
        // Inlined take + in-loop Make*: S2l win-or-gate keeps fuse-IL
        // (residual boxing still loses to invert+fuse).
        let src = r#"
fn take([int] xs) -> int {
    return xs[0];
}
fn hot(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + take([i, i + 1]);
        i = i + 1;
    }
    return s;
}
fn main() {
    if hot(3) != 3 {
        panic "hot checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile alloc loop");
        let symbols = p.program_debug().fn_symbols;
        let hot = symbols
            .iter()
            .position(|s| s.name == "hot")
            .expect("hot symbol");
        let start = symbols[hot].entry_pc as usize;
        let end = symbols
            .get(hot + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let hot_bc = &bc[start..end];
        let names: Vec<_> = hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect();
        let has_make = hot_bc.iter().any(|b| *b.bytecode() == Instruction::MakeArray);
        let has_dense_make = hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseMake);
        if has_make {
            assert!(
                hot_bc.iter().all(|b| *b.bytecode() != Instruction::DenseBin),
                "residual MakeArray stays fuse-IL; opcodes={names:?}"
            );
        }
        if has_dense_make {
            assert!(
                hot_bc.iter().all(|b| *b.bytecode() != Instruction::MakeArray),
                "A2 DenseMake is not residual-boxed; opcodes={names:?}"
            );
        }
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "hot checksum; opcodes={names:?}");
    }

    #[test]
    fn s2l_inloop_escape_make_stays_fuse_or_dense_native() {
        // Escaping `[i, i+1]` cannot SROA. A2 keeps dense only when Make*
        // is DenseMake (no LOAD/StorePop boxing); else fuse-IL.
        let src = r#"
fn take([int] xs) -> int {
    return xs[0] + xs[1];
}
fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + take([i, i + 1]);
        i = i + 1;
    }
    return s;
}
fn main() {
    if pack(5) != 25 {
        panic "s2l escape checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile escape loop");
        let symbols = p.program_debug().fn_symbols;
        let pack = symbols
            .iter()
            .position(|s| s.name == "pack")
            .expect("pack symbol");
        let start = symbols[pack].entry_pc as usize;
        let end = symbols
            .get(pack + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let pack_bc = &bc[start..end];
        let names: Vec<_> = pack_bc.iter().map(|b| b.bytecode().mnemonic()).collect();
        let has_make = pack_bc
            .iter()
            .any(|b| *b.bytecode() == Instruction::MakeArray);
        let has_dense_make = pack_bc
            .iter()
            .any(|b| *b.bytecode() == Instruction::DenseMake);
        if has_make {
            assert!(
                pack_bc
                    .iter()
                    .all(|b| *b.bytecode() != Instruction::DenseBin),
                "S2l: boxed in-loop Make* stays fuse-IL; opcodes={names:?}"
            );
        }
        if has_dense_make {
            assert!(
                pack_bc.iter().all(|b| *b.bytecode() != Instruction::MakeArray),
                "A2: DenseMake must not also residual-box MakeArray; opcodes={names:?}"
            );
        }
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "s2l escape checksum; opcodes={names:?}");
    }

    #[test]
    fn s2c_mapped_pair_helper_takes_lir() {
        let src = r#"
fn pair(int a, int b) -> [int] {
    return [a, b];
}
fn main() {
    let xs = pair(3, 4);
    let _ = xs[0] + xs[1];
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile pair");
        assert!(
            !p.stack_maps().is_empty(),
            "mapped pair/main should emit S2b maps: {:?}",
            p.stack_maps()
        );
        let symbols = p.program_debug().fn_symbols;
        let pair = symbols
            .iter()
            .position(|s| s.name == "pair")
            .expect("pair symbol");
        let start = symbols[pair].entry_pc as usize;
        let end = symbols
            .get(pair + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let pair_bc = &bc[start..end];
        assert!(
            pair_bc
                .iter()
                .any(|b| *b.bytecode() == Instruction::MakeArray),
            "pair keeps MakeArray; opcodes={:?}",
            pair_bc
                .iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        assert!(
            pair_bc
                .iter()
                .all(|b| *b.bytecode() != Instruction::DenseBin),
            "S3: pair stays LIR MakeArray (not dense+match)"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn s2d_mapped_looping_makearray_takes_dense() {
        // S2f SROA: computed-index `[T; N]` is slots, not in-loop MakeArray.
        let src = r#"
fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [i, i + 1, i + 2];
        s = s + xs[i % 3];
        i = i + 1;
    }
    return s;
}
fn main() {
    if pack(4) != 9 {
        panic "pack checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile pack");
        let symbols = p.program_debug().fn_symbols;
        let pack = symbols
            .iter()
            .position(|s| s.name == "pack")
            .expect("pack");
        let start = symbols[pack].entry_pc as usize;
        let end = symbols
            .get(pack + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let pack_bc = &bc[start..end];
        let names: Vec<_> = pack_bc.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            pack_bc
                .iter()
                .all(|b| *b.bytecode() != Instruction::MakeArray),
            "S2f SROA drops in-loop MakeArray; opcodes={names:?}"
        );
        assert!(
            pack_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S2k: pack select load takes dense; opcodes={names:?}"
        );
        let seek = pack_bc
            .iter()
            .filter(|b| *b.bytecode() == Instruction::Seek)
            .map(|b| b.operand_u32())
            .max()
            .unwrap_or(0);
        assert!(seek <= 64, "S2k pack Seek {seek} overflows 64-slot frame");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "pack checksum; opcodes={names:?}");
    }

    #[test]
    fn s2d_mapped_preheader_array_mut_takes_dense() {
        let src = r#"
fn bump() -> int {
    let arr = [0, 0, 0, 0];
    let i = 0;
    while i < 8 {
        arr[i % 4] = arr[i % 4] + 1;
        i = i + 1;
    }
    return arr[0] + arr[1] + arr[2] + arr[3];
}
fn main() {
    if bump() != 8 {
        panic "bump checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile bump");
        let symbols = p.program_debug().fn_symbols;
        let bump = symbols
            .iter()
            .position(|s| s.name == "bump")
            .expect("bump");
        let start = symbols[bump].entry_pc as usize;
        let end = symbols
            .get(bump + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        let makes = body
            .iter()
            .filter(|b| *b.bytecode() == Instruction::MakeArray)
            .count();
        assert!(
            makes == 0
                || body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S2f SROA drops MakeArray, or S2d dense keeps it; opcodes={names:?}"
        );
        let seeks = body
            .iter()
            .filter(|b| *b.bytecode() == Instruction::Seek)
            .count();
        assert_eq!(
            makes, 0,
            "S2f SROA drops preheader MakeArray; opcodes={names:?}"
        );
        assert!(
            seeks <= 1,
            "S2f SROA: no residual Seek tax (prologue only if dense); opcodes={names:?}"
        );
        let seek_hw = body
            .iter()
            .filter(|b| *b.bytecode() == Instruction::Seek)
            .map(|b| b.operand_u32())
            .max()
            .unwrap_or(0);
        assert!(
            seek_hw <= p.operand_stack_slots(),
            "dense Seek {seek_hw} exceeds operand stack {}",
            p.operand_stack_slots()
        );
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S2k: bump store-select takes dense; opcodes={names:?} seek={seek_hw}"
        );
        let slots = p.operand_stack_slots() as usize;
        let mut vm = machine::Machine::<256>::with_operand_capacity(slots);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "bump checksum; opcodes={names:?}");
    }

    #[test]
    fn s2k_pack_store_select_takes_dense() {
        let src = r#"
fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [0, 0, 0];
        xs[i % 3] = i;
        s = s + xs[i % 3];
        i = i + 1;
    }
    return s;
}
fn main() {
    if pack(6) != 15 {
        panic "pack_store checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile pack_store");
        let symbols = p.program_debug().fn_symbols;
        let pack = symbols
            .iter()
            .position(|s| s.name == "pack")
            .expect("pack");
        let start = symbols[pack].entry_pc as usize;
        let end = symbols
            .get(pack + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().all(|b| *b.bytecode() != Instruction::MakeArray),
            "S2k pack_store stays slots; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S2k pack_store select takes dense; opcodes={names:?}"
        );
        let seek = body
            .iter()
            .filter(|b| *b.bytecode() == Instruction::Seek)
            .map(|b| b.operand_u32())
            .max()
            .unwrap_or(0);
        assert!(seek <= 64, "S2k pack_store Seek {seek} overflows 64-slot frame");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "pack_store checksum; opcodes={names:?}");
    }

    #[test]
    fn s2d_mapped_looping_makearray_lir_when_compare_only() {
        let src = r#"
fn pick(int go, int k) -> int {
    let xs = [1, 2, 3];
    while go != 0 {
        xs = [4, 5, 6];
        go = 0;
    }
    return xs[k];
}
fn main() {
    if pick(1, 1) != 5 || pick(0, 2) != 6 {
        panic "pick checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile spin");
        let symbols = p.program_debug().fn_symbols;
        let pick = symbols
            .iter()
            .position(|s| s.name == "pick")
            .expect("pick");
        let start = symbols[pick].entry_pc as usize;
        let end = symbols
            .get(pick + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        let makes = body
            .iter()
            .filter(|b| *b.bytecode() == Instruction::MakeArray)
            .count();
        assert!(
            makes <= 1,
            "S2f SROA: at most OOB MakeArray for xs[k]; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| *b.bytecode() != Instruction::DenseBin),
            "compare-only looping alloc stays LIR; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "pick checksum; opcodes={names:?}");
    }

    #[test]
    fn s3_vec_scan_fill_takes_v0_simd() {
        let src = r#"
fn fill(Vec<int> v) -> int {
    let i = 0;
    while i < len(v) {
        v[i] = i;
        i = i + 1;
    }
    return len(v);
}
fn main() {
    let v: Vec<int> = Vec::from([1, 2, 3]);
    let _ = fill(v);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile fill");
        let symbols = p.program_debug().fn_symbols;
        let fill_i = symbols
            .iter()
            .position(|s| s.name == "fill")
            .expect("fill");
        let start = symbols[fill_i].entry_pc as usize;
        let end = symbols
            .get(fill_i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let fill_bc = &bc[start..end];
        let names: Vec<_> = fill_bc.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            fill_bc.iter().any(|b| *b.bytecode() == Instruction::VStore),
            "S5a V0: stride-1 fill emits VStore; opcodes={names:?}"
        );
        assert!(
            fill_bc.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::StoreIndex
                    | Instruction::StoreIndexUnchecked
                    | Instruction::StoreIndexPin
                    | Instruction::StoreIndexPinUnchecked
                    | Instruction::DenseStoreIndex
            )),
            "scalar tail must keep a heap store; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn s3_index_loop_takes_dense() {
        let src = r#"
fn sum(Vec<int> arr) -> int {
    let i = 0;
    let s = 0;
    while i < len(arr) {
        s = s + arr[i];
        i = i + 1;
    }
    return s;
}
fn main() {
    let v: Vec<int> = Vec::from([1, 2, 3, 4]);
    if sum(v) != 10 {
        panic "indexed_sum checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile index loop");
        let sum = p.function_offset("sum").expect("sum");
        let main = p.function_offset("main").expect("main");
        let sum_bc = if sum < main { &bc[sum..main] } else { &bc[sum..] };
        assert!(
            sum_bc.iter().any(|b| *b.bytecode() == Instruction::VReduce),
            "S5b V1: Vec add-reduce emits VReduce; opcodes={:?}",
            sum_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            sum_bc.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::Index
                    | Instruction::IndexUnchecked
                    | Instruction::IndexPin
                    | Instruction::IndexPinUnchecked
                    | Instruction::DenseIndex
            )),
            "scalar tail keeps Index; opcodes={:?}",
            sum_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "indexed_sum checksum");
    }

    #[test]
    fn s3b_reverse_index_takes_dense_and_checksums() {
        let src = r#"
fn sum(Vec<int> arr) -> int {
    let n = len(arr);
    let i = 0;
    let s = 0;
    while i < n {
        s = s + arr[n - 1 - i];
        i = i + 1;
    }
    return s;
}
fn main() {
    let v: Vec<int> = Vec::from([1, 2, 3, 4]);
    if sum(v) != 10 {
        panic "reverse-index checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile reverse-index");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols
            .iter()
            .position(|s| s.name == "sum")
            .expect("sum");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S3b: reverse index takes dense; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| *b.bytecode() != Instruction::VReduce),
            "gather is not V1; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::Index
                    | Instruction::IndexUnchecked
                    | Instruction::DenseIndex
            )),
            "S3b: dense-native or unpinned Index; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| !matches!(
                *b.bytecode(),
                Instruction::IndexPin
                    | Instruction::IndexPinUnchecked
                    | Instruction::ArrayPin
            )),
            "dense Seek cannot keep pin keys; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "reverse-index checksum; opcodes={names:?}");
    }

    #[test]
    fn s3_times_a_is_dense_call_and_index() {
        let src = r#"
fn eval_a(int i, int j) -> float {
    let ij = i + j;
    let t = (ij * (ij + 1)) / 2 + i + 1;
    return 1.0 / (t as float);
}
fn times_a(Vec<float> v, Vec<float> out) -> float {
    let n = len(v);
    let i = 0;
    while i < n {
        let s = 0.0;
        let j = 0;
        while j < n {
            s = s + eval_a(i, j) * v[j];
            j = j + 1;
        }
        out[i] = s;
        i = i + 1;
    }
    return out[0];
}
fn main() {
    let v: Vec<float> = Vec::from([1.0, 2.0]);
    let out: Vec<float> = Vec::from([0.0, 0.0]);
    let x = times_a(v, out);
    if x != 2.0 {
        panic "times_a checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile times_a");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols
            .iter()
            .position(|s| s.name == "times_a")
            .expect("times_a");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S3b: times_a index+CALL takes dense; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::CALL),
            "open CALL to eval_a stays in the dense body; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::Index
                    | Instruction::IndexUnchecked
                    | Instruction::IndexPin
                    | Instruction::IndexPinUnchecked
                    | Instruction::DenseIndex
            )),
            "times_a keeps Index; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::StoreIndex
                    | Instruction::StoreIndexUnchecked
                    | Instruction::StoreIndexPin
                    | Instruction::StoreIndexPinUnchecked
                    | Instruction::DenseStoreIndex
            )),
            "times_a keeps StoreIndex; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "times_a checksum; opcodes={names:?}");
    }

    #[test]
    fn s3_nsieve_index_store_or_fuse_il() {
        let src = r#"
fn nsieve(int n) -> int {
    let flags: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        flags.push(1);
        i = i + 1;
    }
    let count = 0;
    let p = 2;
    while p < n {
        if flags[p] == 1 {
            count = count + 1;
            let k = p + p;
            while k < n {
                flags[k] = 0;
                k = k + p;
            }
        }
        p = p + 1;
    }
    return count;
}
fn main() {
    if nsieve(16) != 6 {
        panic "nsieve";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile nsieve");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols
            .iter()
            .position(|s| s.name == "nsieve")
            .expect("nsieve");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "D0: nsieve keeps dense after lift; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::Index
                    | Instruction::IndexUnchecked
                    | Instruction::DenseIndex
            )),
            "nsieve keeps Index; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::StoreIndex
                    | Instruction::StoreIndexUnchecked
                    | Instruction::DenseStoreIndex
            )),
            "nsieve keeps StoreIndex; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| !matches!(
                *b.bytecode(),
                Instruction::IndexPin
                    | Instruction::IndexPinUnchecked
                    | Instruction::ArrayPin
            )),
            "dense Seek cannot keep pin keys; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "nsieve checksum; opcodes={names:?}");
    }

    #[test]
    fn i7_deopt_edges_visible_and_emit_skips() {
        let loc = DebugLoc {
            file: 0,
            start_byte: 0,
            end_byte: 4,
        };
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut hints = LowerHints::new("dbg");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.param_count = 1;
        hints.allow_deopt = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower deopt");
        f.verify().unwrap();
        assert!(f.has_deopt_edge());
        assert!(
            f.blocks.iter().any(|b| {
                b.insts.iter().any(|i| {
                    matches!(
                        i,
                        MirInst::Deopt {
                            kind: MirDeoptKind::Stop,
                            ..
                        }
                    )
                })
            }),
            "Return is a stop edge"
        );
        let text = f.to_string();
        assert!(text.contains("deopt.stop"), "{text}");
        let g = parse_func(&text).expect(&text);
        g.verify().unwrap();
        assert!(g.has_deopt_edge());
        let mut pool = Vec::new();
        let dense = emit_dense(&f, Some(Label(0)), &mut pool, false).expect("skip Deopt");
        let lir = emit_lir(&f, Some(Label(0)), &mut pool, false).expect("skip Deopt");
        assert!(
            !dense.iter().any(|op| matches!(op, IlOp::HostInvoke { .. })),
            "Deopt must not encode as a host/native"
        );
        assert!(!lir.is_empty());
        let mut no = LowerHints::new("plain");
        no.slot_ty.insert(0, MirTy::I64);
        no.param_count = 1;
        let plain = try_lower_numeric(&ops, &no).expect("plain");
        assert!(!plain.has_deopt_edge());
    }

    #[test]
    fn pipeline_debugger_attached_and_og_may_dense() {
        let src = r#"
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf + a - b;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(2.0, 1.0, 8);
}
"#;
        let mut def = crate::Pipeline::new();
        let (bc_def, constants) = def.compile_src(src).expect("compile default");
        assert!(
            bc_def.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "default must dense hot; opcodes={:?}",
            bc_def.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );

        let mut attached = crate::Pipeline::new();
        attached.set_debugger_attached(true);
        let (bc, constants_dbg) = attached
            .compile_src(src)
            .expect("compile debugger-attached");
        assert!(
            attached.debugger_attached(),
            "flag must stick"
        );
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "B8 debugger-attached may dense; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert_eq!(
            bc, bc_def,
            "debugger-attached must not change Standard reconstruct"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants_dbg, attached.strings(), attached.static_slot_count());
        let mut vm_def = machine::Machine::<64>::with_operand_capacity(64);
        vm_def.run_raw(&bc_def, &constants, def.strings(), def.static_slot_count());

        let mut og = crate::Pipeline::new();
        og.set_opt_level(crate::OptLevel::Debug);
        let (bc_og, constants_og) = og.compile_src(src).expect("compile -Og");
        assert!(
            bc_og.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "B8 -Og may dense; opcodes={:?}",
            bc_og.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let mut vm_og = machine::Machine::<64>::with_operand_capacity(64);
        vm_og.run_raw(&bc_og, &constants_og, og.strings(), og.static_slot_count());
    }

    #[test]
    fn c3_deopt_maps_named_lets_and_sparse_locs() {
        let src = r#"
fn hot(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(8);
}
"#;
        let mut p = crate::Pipeline::new();
        p.set_debugger_attached(true);
        let (bc, constants) = p.compile_src(src).expect("compile");
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "C3 board must dense; opcodes={:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let drafts = p.deopt_map_drafts();
        assert!(
            drafts.iter().any(|d| d.name.contains("hot") && !d.sites.is_empty()),
            "C3 deopt maps on hot; drafts={drafts:?}"
        );
        let locals = p.fn_debug_locals();
        let hot = locals
            .iter()
            .find(|(n, _)| n.contains("hot"))
            .map(|(_, l)| l.as_slice())
            .unwrap_or(&[]);
        assert!(
            hot.iter().any(|(n, _)| n == "n"),
            "param n stays named; locals={hot:?}"
        );
        assert!(
            hot.iter().any(|(n, _)| n == "i" || n == "s"),
            "named lets survive remap; locals={hot:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn emit_dense_forwards_known_debugloc() {
        let loc = DebugLoc {
            file: 0,
            start_byte: 10,
            end_byte: 20,
        };
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut hints = LowerHints::new("add1");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.param_count = 1;
        let f = try_lower_numeric(&ops, &hints).expect("lower");
        assert!(
            f.value_locs.values().any(|l| l.is_known()),
            "lower must keep known dest locs"
        );
        let mut pool = Vec::new();
        let dense = emit_dense(&f, Some(Label(0)), &mut pool, false).expect("emit");
        assert!(
            dense.iter().any(|op| op.loc().is_known()),
            "C3 dense emit must forward known DebugLoc"
        );
    }

    #[test]
    fn i6_clock_edge_visible_and_s3_dense_emits() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const {
                imm: i32::from(common::CLOCK_MONO_NANOS_ID),
                loc,
            },
            IlOp::HostInvoke {
                arity: 0,
                layout: 0,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        assert!(
            super::infer::infer_numeric(&ops, 0, 0).is_err(),
            "clock-only body still misses the numeric work gate"
        );
        assert!(try_specialize_body(&ops, "clk", 0, &mut pool, &DenseCallMap::new(), None).is_none());
        let mut hints = LowerHints::new("clk");
        hints.allow_effects = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower clock");
        f.verify().unwrap();
        assert!(f.has_impure_host());
        let text = f.to_string();
        assert!(text.contains("host.clock_mono_nanos"), "{text}");
        let g = parse_func(&text).expect(&text);
        g.verify().unwrap();
        assert!(g.has_impure_host());
        assert!(emit_dense(&f, Some(Label(0)), &mut pool, false).is_ok());
        assert!(emit_lir(&f, Some(Label(0)), &mut pool, false).is_err());
    }

    #[test]
    fn pipeline_clock_loop_takes_dense() {
        let src = r#"
use clock::{mono_nanos};
fn hot(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + mono_nanos() + i;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(3);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile clock loop");
        let symbols = p.program_debug().fn_symbols;
        let hot = symbols
            .iter()
            .position(|s| s.name == "hot")
            .expect("hot symbol");
        let start = symbols[hot].entry_pc as usize;
        let end = symbols
            .get(hot + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let hot_bc = &bc[start..end];
        assert!(
            hot_bc
                .iter()
                .any(|b| *b.bytecode() == Instruction::HostInvoke),
            "clock stays a HostInvoke edge; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "S3 clock+arith loop is dense; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn i4_string_format_enter_lir_not_dense() {
        let loc = loc();
        let format_ops = vec![
            IlOp::Label(Label(0)),
            IlOp::String { idx: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Byte {
                byte: Byte::new(Instruction::FORMAT).with_operand_u32(1),
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        assert!(
            super::stackmap::try_build_draft(&format_ops, "fmt", 1, &[], &[]).is_some(),
            "R3 FORMAT encodes maps"
        );
        let lir = try_lower_abi_body(&format_ops, "fmt", 1, &mut pool)
            .expect("Q9 R1 FORMAT enters MIR→LIR");
        assert!(
            lir.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::FORMAT
            )),
            "LIR reconstructs FORMAT"
        );
        assert!(
            lir.iter().any(|op| matches!(op, IlOp::String { .. })),
            "LIR reconstructs STRING"
        );
        assert!(
            super::infer::infer_numeric(&format_ops, 0, 1).is_err(),
            "FORMAT must not enter dense infer"
        );
        let string_ops = vec![
            IlOp::Label(Label(0)),
            IlOp::String { idx: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let s = try_lower_abi_body(&string_ops, "s", 0, &mut pool)
            .expect("Q9 R1 STRING enters MIR→LIR");
        assert!(s.iter().any(|op| matches!(op, IlOp::String { idx: 0, .. })));
        let print_ops = vec![
            IlOp::Label(Label(0)),
            IlOp::String { idx: 1, loc },
            IlOp::Print { loc },
            IlOp::Const { imm: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let p = try_lower_abi_body(&print_ops, "hello", 0, &mut pool)
            .expect("Q9 R1 PRINT enters MIR→LIR");
        assert!(p.iter().any(|op| matches!(op, IlOp::Print { .. })));
        assert!(p.iter().any(|op| matches!(op, IlOp::String { .. })));
    }

    #[test]
    fn pipeline_format_loop_stays_fuse_il() {
        let src = r#"
use string::{format};
fn hot(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let _ = format("%i", i);
        s = s + i;
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = hot(4);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile format loop");
        let symbols = p.program_debug().fn_symbols;
        let hot = symbols
            .iter()
            .position(|s| s.name == "hot")
            .expect("hot symbol");
        let start = symbols[hot].entry_pc as usize;
        let end = symbols
            .get(hot + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let hot_bc = &bc[start..end];
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::FORMAT),
            "R3 keeps FORMAT reconstruct; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc
                .iter()
                .all(|b| *b.bytecode() != Instruction::DenseBin),
            "R3 must not dense-specialize a FORMAT loop"
        );
        assert!(
            p.stack_maps()
                .iter()
                .any(|m| m.safepoints.iter().any(|s| {
                    hot_bc
                        .get((s.pc as usize).saturating_sub(start))
                        .is_some_and(|b| *b.bytecode() == Instruction::FORMAT)
                })),
            "R3 binds FORMAT safepoints: {:?}",
            p.stack_maps()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_format_loop_maps_live_string() {
        let src = r#"
use string::{format};
fn hot(string keep, int n) -> string {
    let i = 0;
    while i < n {
        let _ = format("%s %i", keep, i);
        i = i + 1;
    }
    return keep;
}
fn main() {
    let _ = hot("k", 3);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile mapped format");
        assert!(
            p.stack_maps().iter().any(|m| {
                m.safepoints
                    .iter()
                    .any(|s| s.slots.contains(&0) && {
                        let i = s.pc as usize;
                        i < bc.len() && *bc[i].bytecode() == Instruction::FORMAT
                    })
            }),
            "live string slot 0 must be rooted at FORMAT: {:?}",
            p.stack_maps()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    fn string_bytes_native_id(name: &str) -> i32 {
        i32::from(
            common::HOST_NATIVES
                .iter()
                .find(|n| n.name == name)
                .expect(name)
                .id,
        )
    }

    fn hot_slice<'a>(
        p: &crate::Pipeline,
        bc: &'a [common::Byte],
    ) -> &'a [common::Byte] {
        let symbols = p.program_debug().fn_symbols;
        let hot = symbols
            .iter()
            .position(|s| s.name == "hot")
            .expect("hot symbol");
        let start = symbols[hot].entry_pc as usize;
        let end = symbols
            .get(hot + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        &bc[start..end]
    }

    #[test]
    fn q9_r2_bytes_host_reconstructs_dense() {
        let loc = loc();
        for name in ["to_bytes", "from_bytes"] {
            let layout = if name == "from_bytes" {
                common::HOST_ENUM_LAYOUT_RESULT_NICHE as u8
            } else {
                0
            };
            let ops = vec![
                IlOp::Label(Label(0)),
                IlOp::Const {
                    imm: string_bytes_native_id(name),
                    loc,
                },
                IlOp::Load { slot: 0, loc },
                IlOp::HostInvoke {
                    arity: 1,
                    layout,
                    loc,
                },
                IlOp::Return { loc, ret_words: 1 },
            ];
            let mut pool = Vec::new();
            assert!(
                super::infer::infer_numeric(&ops, 0, 1).is_err(),
                "{name}-only body still misses the numeric work gate"
            );
            let mut hints = LowerHints::new(name);
            hints.allow_effects = true;
            hints.param_count = 1;
            hints.slot_ty.insert(0, super::ty::MirTy::I64);
            let f = try_lower_numeric(&ops, &hints).expect(name);
            f.verify().unwrap();
            assert!(f.has_impure_host(), "{name} is IO");
            let dense = emit_dense(&f, Some(Label(0)), &mut pool, false).expect(name);
            assert!(
                dense.iter().any(|op| matches!(
                    op,
                    IlOp::HostInvoke { layout: l, .. } if *l == layout
                )),
                "{name} reconstructs HostInvoke layout {layout}"
            );
            assert!(
                emit_lir(&f, Some(Label(0)), &mut pool, false).is_err(),
                "{name} stays off LIR HostInvoke reconstruct"
            );
        }
    }

    #[test]
    fn pipeline_to_bytes_loop_takes_dense() {
        let src = r#"
use string::{to_bytes};
fn hot(string s, int n) -> int {
    let i = 0;
    let acc = 0;
    while i < n {
        let _ = to_bytes(s);
        acc = acc + i;
        i = i + 1;
    }
    return acc;
}
fn main() {
    let _ = hot("x", 4);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile to_bytes loop");
        let hot_bc = hot_slice(&p, &bc);
        assert!(
            hot_bc
                .iter()
                .any(|b| *b.bytecode() == Instruction::HostInvoke),
            "R2 to_bytes stays HostInvoke; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "R2 to_bytes+arith loop is dense; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc
                .iter()
                .all(|b| *b.bytecode() != Instruction::FORMAT),
            "R2 must not pull FORMAT onto dense"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_from_bytes_loop_takes_dense() {
        let src = r#"
use string::{from_bytes, to_bytes};
fn hot(Vec<byte> b, int n) -> int {
    let i = 0;
    let acc = 0;
    while i < n {
        let _ = from_bytes(b);
        acc = acc + i;
        i = i + 1;
    }
    return acc;
}
fn main() {
    let _ = hot(to_bytes("hi"), 4);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile from_bytes loop");
        let hot_bc = hot_slice(&p, &bc);
        assert!(
            hot_bc
                .iter()
                .any(|b| *b.bytecode() == Instruction::HostInvoke),
            "R2 from_bytes stays HostInvoke; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "R2 from_bytes+arith loop is dense; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc.iter().any(|b| {
                *b.bytecode() == Instruction::HostInvoke
                    && common::host_invoke_enum_layout(b.operand_u32())
                        == common::HOST_ENUM_LAYOUT_RESULT_NICHE
            }),
            "dense from_bytes must keep ResultNiche layout"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn i3_heap_getfield_stays_refused() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::GetField { loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        assert!(
            try_lower_abi_body_with(&ops, "heap_field", 1, &mut pool, &[]).is_none(),
            "escaping / heap GetField must stay fuse-IL"
        );
    }

    #[test]
    fn pipeline_unboxed_class_field_reads() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let p = new Point(3, 4);
    let z = p.x + p.y;
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile unboxed Point");
        let symbols = p.program_debug().fn_symbols;
        let main = symbols
            .iter()
            .position(|s| s.name == "main")
            .expect("main symbol");
        let start = symbols[main].entry_pc as usize;
        let end = symbols
            .get(main + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let main_bc = &bc[start..end];
        assert!(
            main_bc.iter().all(|b| !matches!(
                *b.bytecode(),
                Instruction::InitTyped | Instruction::GetField | Instruction::LoadField
            )),
            "non-escaping Point must unbox; opcodes={:?}",
            main_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_escaping_class_stays_heap() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn take(Point q) -> int {
    return q.x;
}
fn hot() -> int {
    let p = new Point(3, 4);
    return take(p);
}
fn main() {
    let _ = hot();
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile escaping Point");
        let symbols = p.program_debug().fn_symbols;
        let hot = symbols
            .iter()
            .position(|s| s.name == "hot")
            .expect("hot symbol");
        let start = symbols[hot].entry_pc as usize;
        let end = symbols
            .get(hot + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let hot_bc = &bc[start..end];
        assert!(
            hot_bc
                .iter()
                .any(|b| matches!(
                    *b.bytecode(),
                    Instruction::InitTyped | Instruction::DenseMakeObject
                )),
            "escaping named local stays boxed-once; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            p.stack_maps()
                .iter()
                .any(|m| !m.safepoints.is_empty()),
            "D1 InitTyped+field body should bind maps: {:?}",
            p.stack_maps()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_field_hot_keeps_dense_native() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}
fn hot() -> int {
    let p = new Point(3, 4);
    let i = 0;
    let acc = 0;
    while i < 4 {
        acc = acc + p.sum() + p.x;
        i = i + 1;
    }
    return acc;
}
fn main() {
    if hot() != 40 {
        panic "hot checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile field_hot board");
        assert!(
            p.stack_maps()
                .iter()
                .any(|m| !m.safepoints.is_empty()),
            "D2 field_hot InitTyped should bind maps: {:?}",
            p.stack_maps()
        );
        let symbols = p.program_debug().fn_symbols;
        for name in ["hot", "Point::sum"] {
            let Some(i) = symbols.iter().position(|s| s.name == name) else {
                continue;
            };
            let start = symbols[i].entry_pc as usize;
            let end = symbols
                .get(i + 1)
                .map(|s| s.entry_pc as usize)
                .unwrap_or(bc.len());
            let slice = &bc[start..end];
            assert!(
                slice
                    .iter()
                    .any(|b| *b.bytecode() == Instruction::DenseBin),
                "{name} must keep DenseBin; opcodes={:?}",
                slice.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
            );
            assert!(
                slice.iter().any(|b| *b.bytecode() == Instruction::DenseFieldLoad),
                "{name} must keep DenseFieldLoad; opcodes={:?}",
                slice.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
            );
        }
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_field_store_loop_keeps_dense() {
        let src = r#"
class Point {
    pub n: int,
}
fn hot() -> int {
    let p = new Point(0);
    let i = 0;
    while i < 4 {
        p.n = p.n + 1;
        i = i + 1;
    }
    return p.n;
}
fn main() {
    if hot() != 4 {
        panic "store checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile field store loop");
        let hot_bc = hot_slice(&p, &bc);
        assert!(
            hot_bc
                .iter()
                .any(|b| *b.bytecode() == Instruction::DenseFieldLoad),
            "field store loop DenseFieldLoad; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            hot_bc
                .iter()
                .any(|b| *b.bytecode() == Instruction::DenseFieldStore),
            "field store loop DenseFieldStore; opcodes={:?}",
            hot_bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn pipeline_result_int_stays_two_slot() {
        let src = r#"
fn checked_div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}
fn main() {
    let ok = match checked_div(6, 3) {
        Result::Ok(q) => q,
        Result::Err(e) => e,
    };
    let err = match checked_div(1, 0) {
        Result::Ok(q) => q,
        Result::Err(e) => e,
    };
    let _ = ok + err;
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile result helper");
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::MakeEnum),
            "two-slot Result must not box; opcodes={:?}",
            bc.iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::CALL && b.call_ret_words() >= 2),
            "direct CALL must stay two-slot"
        );
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::RETURN && b.operand_u32() == 2),
            "RETURN width 2"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
    }

    #[test]
    fn q8_local_option_match_loop_checksum() {
        let src = r#"
fn acc(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + match Option::Some(i) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        i = i + 1;
    }
    return s;
}
fn main() {
    let _ = acc(4);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile Q8 local match");
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::MakeEnum),
            "local Option<int> match must stay two-slot; opcodes={:?}",
            bc.iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "Q8 local match must run");
    }

    #[test]
    fn i2_boxed_overlap_arity0_enters_lir() {
        let loc = loc();
        // Boxed `match o { Some(v) => v, None => 0 }` — codegen emits
        // JumpIfMatch arity 0 (payload via stack/local overlap).
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Byte {
                byte: Byte::new(Instruction::Unpack).with_operand_u32(0),
                loc,
            },
            IlOp::Const { imm: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Label(Label(2)),
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "score_opt", 1, &mut pool)
            .expect("I2 boxed-overlap JumpIfMatch");
        assert!(
            lir.iter().any(|op| matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfMatch { tag: 1, .. },
                    ..
                }
            )),
            "LIR must keep JumpIfMatch"
        );
    }

    #[test]
    fn i2_match_enum_loop_helpers_enter_lir() {
        let src = r#"
enum Phase {
    Low(int),
    Mid(int),
    High(int),
}
fn wrap_opt(int x) -> Option<int> {
    if x % 3 == 0 {
        return Option::None;
    }
    return Option::Some(x);
}
fn wrap_res(int x) -> Result<int, string> {
    if x % 5 == 0 {
        return Result::Err("miss");
    }
    return Result::Ok(x % 7);
}
fn wrap_phase(int x) -> Phase {
    let k = x % 3;
    if k == 0 {
        return Phase::Low(x);
    }
    if k == 1 {
        return Phase::Mid(x);
    }
    return Phase::High(x);
}
fn score_opt(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}
fn score_res(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}
fn score_phase(Phase p) -> int {
    return match p {
        Phase::Low(v) => v,
        Phase::Mid(x) => x + 1,
        Phase::High(y) => y + 2,
    };
}
fn main() {
    let acc = score_opt(wrap_opt(1)) + score_res(wrap_res(1)) + score_phase(wrap_phase(1));
    let _ = acc;
}
"#;
        let dir = std::env::temp_dir();
        let path = dir.join("coi302_match_enum_loop.hy");
        std::fs::write(&path, src).expect("write src");
        let mut p = crate::Pipeline::new();
        let arts = p
            .compile_dissect(path.to_str().unwrap(), true)
            .expect("compile match helpers");
        let snap = arts.il.as_ref().expect("il snapshot");
        let mut module = crate::il::IlModule::from_flat(snap.ops(), snap.funcs());
        let opts = crate::il::opt::OptimizeOptions::default();
        let mut pool = arts.constants.clone();
        let mut per = opts.clone();
        per.multi_op_join_convoy = false;
        per.invert_guard_branch = false;
        per.slot_promote_tell = false;
        per.seek_back_edge = false;
        per.ssa_gvn = false;
        let mut next_label = 1u32;
        let mut entered = Vec::new();
        for body in &mut module.funcs {
            crate::il::opt::optimize_at_with_labels(
                &mut body.ops,
                &per,
                body.meta.entry_sp as i32,
                &mut pool,
                &mut next_label,
            );
            let refuse = lir_refuse(&body.ops, &body.meta.unboxed_fields);
            let lir = try_lower_abi_body_with(
                &body.ops,
                &body.meta.name,
                body.meta.entry_sp,
                &mut pool,
                &body.meta.unboxed_fields,
            );
            match body.meta.name.as_str() {
                "wrap_opt" | "wrap_phase" | "score_opt" | "score_res" | "score_phase" => {
                    assert_eq!(refuse, None, "{} should be I2/I8 eligible", body.meta.name);
                    assert!(
                        lir.is_some(),
                        "{} must lower to LIR",
                        body.meta.name
                    );
                    entered.push(body.meta.name.clone());
                }
                "wrap_res" => {
                    assert_eq!(refuse, None, "Q9 R1: STRING is not a LIR wall");
                    let _ = lir;
                }
                "main" => {
                    assert_ne!(
                        refuse,
                        Some(LirRefuse::Call),
                        "B3: two-slot helper CALL is not a LIR wall"
                    );
                }
                _ => {}
            }
        }
        assert_eq!(
            entered.len(),
            5,
            "construct+match helpers must enter MIR: {entered:?}"
        );

        let mut p2 = crate::Pipeline::new();
        let (bc, constants) = p2.compile_src(src).expect("compile helpers");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        vm.run_raw(&bc, &constants, p2.strings(), p2.static_slot_count());
        assert!(!vm.panicked(), "match helpers must run");
    }

    #[test]
    fn i2_last_arm_unpack_overlap_uses_payload() {
        // Fuse shape of `score_phase`: two arity-0 JIMs then last-arm
        // `Unpack` + overlap `LOAD` + `y + 2`.
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
                target: Label(2),
                loc,
                hint: Default::default(),
            },
            IlOp::Byte {
                byte: Byte::new(Instruction::Unpack).with_operand_u32(1),
                loc,
            },
            IlOp::Load { slot: 1, loc },
            IlOp::Const { imm: 2, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(2)),
            IlOp::Load { slot: 1, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut pool = Vec::new();
        let lir = try_lower_abi_body(&ops, "score_phase", 1, &mut pool)
            .expect("last-arm Unpack overlap");
        assert!(
            lir.iter().any(|op| matches!(
                op,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Unpack
            )),
            "last arm must emit Unpack (miss TOS is still the scrutinee)"
        );
        assert!(
            lir.iter().any(|op| matches!(
                op,
                IlOp::Bin { .. } | IlOp::BinSlotImm { .. }
            )),
            "last arm must keep y + 2"
        );
    }

    #[test]
    fn q6_for_in_array_sum_takes_vreduce() {
        let src = r#"
fn sum(Vec<int> arr) -> int {
    let acc = 0;
    for x in arr {
        acc = acc + x;
    }
    return acc;
}
fn main() {
    let v: Vec<int> = Vec::from([1, 2, 3, 4]);
    if sum(v) != 10 {
        panic "for-in checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile for-in sum");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols.iter().position(|s| s.name == "sum").expect("sum");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::VReduce),
            "Q6 counted for-in should VReduce; opcodes={names:?}"
        );
        assert!(
            body.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::Index
                    | Instruction::IndexUnchecked
                    | Instruction::IndexPin
                    | Instruction::IndexPinUnchecked
                    | Instruction::DenseIndex
            )),
            "scalar tail keeps Index; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "for-in checksum");
    }

    #[test]
    fn q6_for_in_range_sum_takes_dense() {
        let src = r#"
fn range_sum(int n) -> int {
    let acc = 0;
    for x in 0..n {
        acc = acc + x;
    }
    return acc;
}
fn main() {
    if range_sum(5) != 10 {
        panic "range checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile range for-in");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols
            .iter()
            .position(|s| s.name == "range_sum")
            .expect("range_sum");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "Q6 literal range for-in should DenseBin; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| !matches!(
                *b.bytecode(),
                Instruction::MakeDict | Instruction::GetField | Instruction::STRING
            )),
            "literal 0..n must not box a range dict; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "range checksum");
    }

    #[test]
    fn q6_for_in_range_value_sum_takes_dense() {
        let src = r#"
fn range_sum(int n) -> int {
    let r = 0..n;
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}
fn main() {
    if range_sum(5) != 10 {
        panic "range value checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile first-class range for-in");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols
            .iter()
            .position(|s| s.name == "range_sum")
            .expect("range_sum");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "B5 first-class range for-in should DenseBin; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| !matches!(
                *b.bytecode(),
                Instruction::MakeDict | Instruction::GetField | Instruction::STRING
            )),
            "let r = 0..n must not box a range dict in the helper; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "range value checksum");
    }

    #[test]
    fn q6_for_in_range_param_sum_takes_dense() {
        let src = r#"
fn range_sum(Range<int> r) -> int {
    let iter = r;
    let acc = 0;
    for x in iter {
        acc = acc + x;
    }
    return acc;
}
fn main() {
    if range_sum(0..5) != 10 {
        panic "range param checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile param range for-in");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols
            .iter()
            .position(|s| s.name == "range_sum")
            .expect("range_sum");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "C2 param range for-in should DenseBin; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| !matches!(
                *b.bytecode(),
                Instruction::MakeDict | Instruction::GetField | Instruction::STRING
            )),
            "Range param must not GetField in the helper; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "range param checksum");
    }

    #[test]
    fn q6_for_in_range_ret_sum_takes_dense() {
        let src = r#"
fn make_range(int n) -> Range<int> {
    return 0..n;
}
fn range_sum(int n) -> int {
    let r = make_range(n);
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}
fn main() {
    if range_sum(5) != 10 {
        panic "range ret checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile returned range for-in");
        let symbols = p.program_debug().fn_symbols;
        let i = symbols
            .iter()
            .position(|s| s.name == "range_sum")
            .expect("range_sum");
        let start = symbols[i].entry_pc as usize;
        let end = symbols
            .get(i + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bc.len());
        let body = &bc[start..end];
        let names: Vec<_> = body.iter().map(|b| b.bytecode().mnemonic()).collect();
        assert!(
            body.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "C2 returned range for-in should DenseBin; opcodes={names:?}"
        );
        assert!(
            body.iter().all(|b| !matches!(
                *b.bytecode(),
                Instruction::MakeDict | Instruction::GetField | Instruction::STRING
            )),
            "returned Range bind must not GetField in the helper; opcodes={names:?}"
        );
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "range ret checksum");
    }

    #[test]
    fn q6_for_in_range_assign_to_x_keeps_trip_count() {
        let src = r#"
fn trips() -> int {
    let n = 0;
    for x in 0..3 {
        x = 100;
        n = n + 1;
    }
    return n;
}
fn main() {
    if trips() != 3 {
        panic "range assign checksum";
    }
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, constants) = p.compile_src(src).expect("compile range assign");
        let mut vm = machine::Machine::<64>::with_operand_capacity(64);
        p.wire_host_natives(&mut vm);
        vm.run_raw(&bc, &constants, p.strings(), p.static_slot_count());
        assert!(!vm.panicked(), "range assign checksum");
    }
}
