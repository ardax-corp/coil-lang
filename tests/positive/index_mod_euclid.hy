// Q4: indexing `i % N` is defined into `0..N` (Euclidean rem).
test("negative rem indexes last slot") {
    let xs = [10, 20, 30];
    let i = 0 - 1;
    assert(xs[i % 3] == 30)?;
}

test("negative rem store hits last slot") {
    let xs = [10, 20, 30];
    let i = 0 - 4;
    xs[i % 3] = 8;
    assert(xs[2] == 8)?;
}
