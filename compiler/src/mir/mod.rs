//! Numeric MIR: type lattice + SSA builder (P0) and dense emit (P1 / COI-268).
//!
//! Specialized numeric loops lower to dense 3-address opcodes. CALL/RETURN
//! keep the existing `Value` word ABI. Classes / heap stay on [`crate::il`].
#![cfg_attr(not(test), allow(dead_code, unused_imports))]

mod builder;
mod emit;
mod func;
mod infer;
mod inst;
mod lower;
mod specialize;
mod text;
mod ty;

pub use builder::{MirBuilder, MirError};
pub use emit::emit_dense;
pub use func::{MirBlock, MirFunc};
pub use inst::{
    BlockId, LocalId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator,
    ValueId,
};
pub use lower::{LowerError, LowerHints, try_lower_numeric};
pub use specialize::try_specialize_body;
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
        let (bc, _) = p.compile_src(src).expect("compile dense kernel");
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::DenseBin),
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
    }

    #[test]
    fn pipeline_leaves_nested_loops_on_stack_il() {
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
        let (bc, _) = p.compile_src(src).expect("compile nested");
        assert!(
            !bc.iter()
                .any(|b| *b.bytecode() == Instruction::DenseBin),
            "nested loops stay on fuse-IL"
        );
    }

    #[test]
    fn pipeline_leaves_int_loop_on_stack_il() {
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
        let (bc, _) = p.compile_src(src).expect("compile int loop");
        assert!(
            !bc.iter()
                .any(|b| *b.bytecode() == Instruction::DenseBin),
            "int-only loops stay on fuse-IL"
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
}
