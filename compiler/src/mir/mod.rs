//! Numeric MIR: type lattice + SSA builder (P0), dense emit (P1), CSE (P2),
//! Result/Option MIR→LIR (P3 / COI-270), LICM (P6 / COI-280),
//! InstCombine (P7 / COI-281), DestProp (P8 / COI-282),
//! IV strength reduction (P9 / COI-283), cross-block GVN/PRE (P10 / COI-284),
//! conservative float peeps (P11 / COI-285), and saxpy-reduce HostInvoke
//! packs (P12 / COI-286).
//!
//! Specialized numeric loops lower to dense 3-address opcodes. Two-slot
//! Option/Result leafs lower back to fuse-IL (`RETURN` width 2). Dense→dense
//! `CALL` uses the one-word typed ABI ([`abi`]; COI-291). Allowlisted
//! HostInvoke (W4) still boxes at the host edge. Classes / heap stay on
//! [`crate::il`].
#![cfg_attr(not(test), allow(dead_code, unused_imports))]

mod abi;
mod builder;
mod cse;
mod destprop;
mod emit;
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
mod strength;
mod text;
mod ty;

pub use abi::{DenseAbi, DenseCallMap};
pub use builder::{MirBuilder, MirError};
pub use cse::{cse, gvn};
pub use destprop::destprop;
pub use emit::emit_dense;
pub use emit_lir::emit_lir;
pub use func::{MirBlock, MirFunc};
pub use inst::{
    BlockId, LocalId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator,
    ValueId,
};
pub use infer::{STRAIGHT_LINE_MIN_WORK_OPS, numeric_work_ops};
pub use instcombine::instcombine;
pub use layout::MirLayout;
pub use licm::licm;
pub use lower::{LowerError, LowerHints, try_lower_numeric};
pub use specialize::{try_lower_abi_body, try_specialize_body};
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
    use common::{DebugLoc, Instruction};

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
        )>();
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
    fn pipeline_refuses_straight_line_below_work_gate() {
        let src = r#"
fn eval_a(int i, int j) -> int {
    return i + j * 2;
}
fn main() {
    let _ = eval_a(1, 2);
}
"#;
        let mut p = crate::Pipeline::new();
        let (bc, _) = p.compile_src(src).expect("compile below-gate i64");
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "straight-line below STRAIGHT_LINE_MIN_WORK_OPS stays fuse-IL"
        );
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
        assert_eq!(STRAIGHT_LINE_MIN_WORK_OPS, 8);
        assert_eq!(numeric_work_ops(&[]), 0);
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "straight-line above work-op gate must emit DenseBin"
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
        t = t * t + x;
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

    fn pipeline_refuses_call_to_non_dense_helper() {
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
        let (bc, _) = p.compile_src(src).expect("compile mid CALL refuse");
        let hot = p.function_offset("hot").expect("hot");
        let main = p.function_offset("main").expect("main");
        let hot_bc = if hot < main { &bc[hot..main] } else { &bc[hot..] };
        assert!(
            !hot_bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "CALL to a non-dense callee must refuse caller specialize"
        );
        assert!(
            hot_bc.iter().any(|b| *b.bytecode() == Instruction::CALL)
                || bc.iter().any(|b| *b.bytecode() == Instruction::CALL),
            "site stays a direct CALL (or was tiny-inlined — then no DenseBin in hot)"
        );
    }

    fn pipeline_refuses_user_call_inside_numeric_loop() {
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
        let (bc, _) = p.compile_src(src).expect("compile user CALL refuse");
        assert!(
            !bc.iter().any(|b| *b.bytecode() == Instruction::DenseBin),
            "user CALL must refuse dense specialize"
        );
        assert!(
            bc.iter().any(|b| *b.bytecode() == Instruction::CALL),
            "negative W4 case keeps a direct CALL"
        );
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
            "nbody eval_a meets STRAIGHT_LINE_MIN_WORK_OPS and emits DenseBin"
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
        let dense = emit_dense(&f, Some(Label(0)), &mut pool).expect("emit");
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
        assert!(emit_dense(&f, Some(Label(0)), &mut pool).is_err());
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
        let lir = emit_lir(&f, Some(Label(0)), &mut pool).expect("emit niche");
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
}
