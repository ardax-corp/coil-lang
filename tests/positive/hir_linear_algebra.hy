// `dot`, `cross` and `matmul` on fixed vectors and matrices, also nested in
// larger expressions.

fn third([int; 3] v) -> int {
    return v[2];
}

test("dot on ints and floats") {
    assert(dot((1, 2, 3), (4, 5, 6)) == 32)?;
    assert(dot([1.5, 2.0], [2.0, 0.25]) == 3.5)?;
    assert(10 + dot([1, 1], [2, 3]) == 15)?;
}

test("cross on arrays and tuples") {
    let c = cross([1, 0, 0], [0, 1, 0]);
    assert(c[0] == 0 && c[1] == 0 && c[2] == 1)?;
    assert(third(cross([0, 1, 0], [0, 0, 1])) == 0)?;
    let t = cross((0, 0, 1), (1, 0, 0));
    assert(t[1] == 1)?;
    assert(1 + third(cross([1, 2, 3], [4, 5, 6])) == -2)?;
}

test("matmul") {
    let m = matmul([[1, 0], [0, 2]], [[3, 4], [5, 6]]);
    assert(m[0][0] == 3 && m[1][1] == 12)?;
}
