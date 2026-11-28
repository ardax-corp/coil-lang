use super::*;

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

/// The copy-on-write pin pair (archive 8.2) reads back from its text and
/// checks: the pin takes an object word and gives a `bool`, the unpin
/// takes that `bool`.
#[test]
fn the_pin_ops_read_back_and_check_their_types() {
    let text = "func @f(v0: heapref) -> heapref {\nbb0:\n    v1 = cow.pin v0\n    v2 = cow.share v0\n    v3 = cow.unpin v0, v1\n    return v2\n}\n";
    let f = parse_func(text).expect(text);
    f.verify().unwrap();
    assert!(f.blocks[0].insts.iter().any(|i| matches!(i, MirInst::CowPin { .. })));
    assert_eq!(parse_func(&f.to_string()).unwrap(), f);
    let bad = "func @g(v0: i64) -> i64 {\nbb0:\n    v1 = cow.pin v0\n    return v0\n}\n";
    assert!(parse_func(bad).expect(bad).verify().is_err(), "a pin of an int");
}

/// Under a debugger, a body with a class local and an array local (which
/// the HIR plan would split into frame slots) still lowers to direct MIR.
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
    p.compile_src(src).expect("compile debugger-attached");
}

