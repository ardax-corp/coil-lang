// `Matrix<D>` is its data at run time: `matrix(d)` is `d`, indexing reads
// the data, and its operators take the linear-algebra kernels.

test("matmul and element-wise add") {
    let a = matrix([[1, 2], [3, 4]]);
    let b = matrix([[5, 6], [7, 8]]);
    let c = a * b;
    assert(c[0][0] == 19 && c[0][1] == 22 && c[1][0] == 43 && c[1][1] == 50)?;
    let d = a + a;
    assert(d[1][1] == 8)?;
}

test("masks and bitwise ops") {
    let a = matrix([[1, 2], [3, 4]]);
    let b = matrix([[1, 0], [3, 9]]);
    let eq = a == b;
    assert(eq[0][0] as int == 1 && eq[0][1] as int == 0)?;
    let bits = a & b;
    assert(bits[1][1] == 0 && bits[1][0] == 3)?;
    let q = matrix([[34 as byte, 0 as byte]]);
    let flipped = ~q;
    assert(flipped[0][1] as int == 255)?;
}

test("negation and float compare") {
    let f = matrix([[1.0, 2.0], [3.0, 4.0]]);
    let g = matrix([[0.0, 2.0], [9.0, 1.0]]);
    let gt = f > g;
    assert(gt[0][0] as int == 1 && gt[1][0] as int == 0)?;
    let n = -f;
    assert(n[1][1] == -4.0)?;
}
