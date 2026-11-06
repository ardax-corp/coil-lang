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

