// Packed linear-algebra kernels reached from direct MIR: `dot` and
// `matmul` calls and the matrix operators (`*`, element-wise `+`, masks,
// negation, `~` on bytes), each one `HostInvoke` of its kernel with the
// shape packed in a meta word.
use tests::assert;
use gc::collect;

fn idot(int k) -> int {
    return dot([1, 2, k], [4, 5, 6]) + 1;
}

fn fdot(float x) -> float {
    return dot([x, 2.0], [2.0, 0.25]);
}

fn mm(int k) -> int {
    let m = matmul([[1, 0], [0, k]], [[3, 4], [5, 6]]);
    collect();
    return m[0][0] + m[1][1];
}

fn ops(int k) -> int {
    let a = matrix([[1, 2], [3, k]]);
    let b = matrix([[5, 6], [7, 8]]);
    let c = a * b;
    let d = a + b;
    collect();
    return c[0][0] + c[1][1] + d[1][1];
}

fn masks(float x) -> int {
    let f = matrix([[1.0, x], [3.0, 4.0]]);
    let g = matrix([[0.0, 2.0], [9.0, 1.0]]);
    let gt = f > g;
    let eq = f == g;
    let n = -f;
    let t = gt[0][0] as int + 2 * (gt[0][1] as int) + 4 * (eq[0][1] as int);
    return t * 10 + (n[1][1] as int);
}

fn flip(byte b) -> int {
    let q = matrix([[b, 0 as byte]]);
    let flipped = ~q;
    return flipped[0][0] as int + flipped[0][1] as int;
}

test("dot on ints and floats") {
    assert!(idot(3) == 33);
    assert!(fdot(1.5) == 3.5);
}

test("matmul call") {
    assert!(mm(2) == 15);
}

test("matrix operators") {
    assert!(ops(4) == 19 + 50 + 12);
    assert!(masks(2.0) == 46);
    assert!(masks(5.0) == 26);
    assert!(flip(34 as byte) == 221 + 255);
}
