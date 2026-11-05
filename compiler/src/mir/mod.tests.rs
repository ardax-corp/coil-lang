use super::*;
use crate::il::{IlJumpKind, IlOp, Label};
use common::{Byte, Instruction};

fn is_dense_bin_op(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::DenseBin | Instruction::DenseBin2 | Instruction::DenseBinJmpf
    )
}

fn fn_ops<'a>(bc: &'a [Byte], symbols: &[common::FnDebugSym], name: &str) -> &'a [Byte] {
    let start = symbols
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("missing fn {name}"))
        .entry_pc as usize;
    let end = symbols
        .iter()
        .filter(|s| (s.entry_pc as usize) > start)
        .map(|s| s.entry_pc as usize)
        .min()
        .unwrap_or(bc.len());
    &bc[start..end]
}

/// The loop runs as one `VLoop`, its body words right after it.
fn assert_vec_loop(ops: &[Byte], fn_name: &str) {
    let names: Vec<_> = ops.iter().map(|b| b.bytecode().mnemonic()).collect();
    let at = ops.iter().position(|b| *b.bytecode() == Instruction::VLoop);
    let Some(at) = at else {
        panic!("{fn_name}: expected a VLoop; opcodes={names:?}");
    };
    let n = usize::from(ops[at].dense_abc_parts().0 & common::simd::LOOP_LEN);
    assert!(n > 0, "{fn_name}: empty VLoop body; opcodes={names:?}");
    assert!(
        ops[at + 1..at + 1 + n].iter().all(|b| matches!(
            *b.bytecode(),
            Instruction::VLoad | Instruction::VStore | Instruction::VBin | Instruction::VFma | Instruction::VReduce
        )),
        "{fn_name}: VLoop body must be V* words; opcodes={names:?}"
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

const BOTTOM_UP: &str = r#"
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

/// Under a debugger, a body with a class local and an array local (which
/// the HIR plan would split into frame slots) still lowers to direct MIR
/// and takes a MIR tier.
#[test]
fn debugger_attached_split_locals_take_direct_mir() {
    let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn sum3(int n) -> int {
    let p = new Point(n, 4);
    let xs = [1, 2, 3];
    let total = 0;
    for x in xs {
        total = total + x + p.x;
    }
    return total + p.y;
}
fn main() {
    let r = sum3(5);
}
"#;
    let mut p = crate::Pipeline::new();
    p.set_debugger_attached(true);
    p.set_collect_opt_stats(true);
    p.compile_src(src).expect("compile debugger-attached");
    let stats = crate::last_opt_stats();
    let sum3 = stats
        .body_tiers
        .iter()
        .find(|b| b.name == "sum3")
        .unwrap_or_else(|| panic!("sum3 tier; stats={stats:?}"));
    assert_eq!(sum3.hir_reason.as_deref(), Some("lowered"), "{sum3:?}");
    assert_ne!(sum3.tier, "fuse", "{sum3:?}");
}

fn is_jmpt(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::BinSlotSlotJmpt | Instruction::BinSlotImmJmpt | Instruction::CmpJmpt | Instruction::JMPT
    )
}

fn hot_slice<'a>(p: &crate::Pipeline, bc: &'a [common::Byte]) -> &'a [common::Byte] {
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

/// `n` arms that rebind a heap word (`s = s + "a"`) into a chain of joins,
/// none of them reached: what a folded `if x == i` leaves when nothing
/// drops the dead arms (#1493).
fn unreachable_rebind_arms(n: usize) -> String {
    let mut text = String::from("func @main() -> i64 {\nbb0:\n    v1 = string 0\n    jump bb2\n");
    let mut s = 1;
    let mut v = 1;
    let mut prev = 0;
    for i in 0..n {
        let (arm, join) = (2 * i + 1, 2 * i + 2);
        let (a, b, c, d, e, phi) = (v + 1, v + 2, v + 3, v + 4, v + 5, v + 6);
        v += 6;
        text.push_str(&format!(
            "bb{arm}:\n    v{a} = string 1\n    v{b} = string 2\n    v{c} = format v{a}, v{s}, v{b}\n    v{d} = gcbarrier.safepoint v{c}, v{s}\n    v{e} = clearword v{s}\n    jump bb{join}\n"
        ));
        text.push_str(&format!("bb{join}:\n    v{phi} = phi.heapref [bb{prev}: v{s}, bb{arm}: v{d}]\n"));
        if i + 1 < n {
            text.push_str(&format!("    jump bb{}\n", join + 2));
        } else {
            text.push_str(&format!("    v{} = iconst.i64 0\n    return v{}\n", v + 1, v + 1));
        }
        s = phi;
        prev = join;
    }
    text.push('}');
    text
}

/// `n` arms that rebind a heap word (`s = s + "a"`) into a chain of joins,
/// each taken or not on `v0`: `if x == k { s = s + "a"; }` (#1502).
fn reachable_rebind_arms(n: usize) -> String {
    let mut text = String::from("func @main(v0: bool) -> i64 {\nbb0:\n    v1 = string 0\n    brif v0, bb1, bb2\n");
    let mut s = 1;
    let mut v = 1;
    let mut prev = 0;
    for i in 0..n {
        let (arm, join) = (2 * i + 1, 2 * i + 2);
        let (a, b, c, d, e, phi) = (v + 1, v + 2, v + 3, v + 4, v + 5, v + 6);
        v += 6;
        text.push_str(&format!(
            "bb{arm}:\n    v{a} = string 1\n    v{b} = string 2\n    v{c} = format v{a}, v{s}, v{b}\n    v{d} = gcbarrier.safepoint v{c}, v{s}\n    v{e} = clearword v{s}\n    jump bb{join}\n"
        ));
        text.push_str(&format!("bb{join}:\n    v{phi} = phi.heapref [bb{prev}: v{s}, bb{arm}: v{d}]\n"));
        if i + 1 < n {
            text.push_str(&format!("    brif v0, bb{}, bb{}\n", join + 1, join + 2));
        } else {
            text.push_str(&format!("    v{} = iconst.i64 0\n    return v{}\n", v + 1, v + 1));
        }
        s = phi;
        prev = join;
    }
    text.push('}');
    text
}

/// `CONST 0; STORE` pairs in LIR: the GC clears ([`emit::push_clear_word`]).
fn lir_clear_words(ops: &[IlOp]) -> usize {
    ops.windows(2)
        .filter(|w| {
            matches!(w[0], IlOp::Const { imm: 0, .. })
                && matches!(&w[1], IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::STORE)
        })
        .count()
}
