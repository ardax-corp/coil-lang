// Element-wise operators on the packed kernel (a static shape of at least
// 8) and unrolled `cross`, on direct MIR.
use tests::assert;

test("packed element-wise operators") {
    let a = [1, 2, 3, 4, 5, 6, 7, 8];
    let k = 3;
    let p = a * k;
    let n = -a;
    let z = a + a;
    assert!(p[7] == 24 && n[0] == -1 && z[4] == 10);
    let f = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0];
    let g = -f;
    let h = f + f;
    assert!(g[1] == -1.0 && h[7] == 8.0);
}

test("unrolled cross on ints and floats") {
    let c = cross([2, 3, 4], [5, 6, 7]);
    assert!(c[0] == -3 && c[1] == 6 && c[2] == -3);
    let t = cross((1.0, 0.0, 0.0), (0.0, 1.0, 0.0));
    assert!(t[2] == 1.0 && t[0] == 0.0);
}
