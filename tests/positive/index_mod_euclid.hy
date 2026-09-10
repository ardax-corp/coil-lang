// Q4: indexing `i % N` is defined into `0..N` (Euclidean rem).
// Last-arm luck would map every negative rem to slot N-1; `(-2) % 3`
// must be slot 1 (20), not slot 2 (30).

test("negative rem indexes last slot") {
    let xs = [10, 20, 30];
    assert(xs[(0 - 1) % 3] == 30)?;
}

test("negative rem that is not last-arm luck") {
    let xs = [10, 20, 30];
    assert(xs[(0 - 2) % 3] == 20)?;
    assert(xs[(0 - 3) % 3] == 10)?;
    assert(xs[(0 - 5) % 3] == 20)?;
}

test("negative rem store hits last slot") {
    let xs = [10, 20, 30];
    xs[(0 - 4) % 3] = 8;
    assert(xs[2] == 8)?;
}

test("negative rem store that is not last-arm luck") {
    let xs = [10, 20, 30];
    xs[(0 - 2) % 3] = 9;
    assert(xs[1] == 9)?;
    assert(xs[2] == 30)?;
}

fn sink([int; 3] xs) {}

test("local dividend uses Euclidean rem") {
    let xs = [10, 20, 30];
    let i = 0 - 2;
    assert(xs[i % 3] == 20)?;
    i = 0 - 5;
    assert(xs[i % 3] == 20)?;
}

test("escaped heap index uses Euclidean rem") {
    let xs = [10, 20, 30];
    sink(xs);
    let i = 0 - 2;
    assert(xs[i % 3] == 20)?;
    i = 0 - 1;
    assert(xs[i % 3] == 30)?;
}

test("plain index without rem still OOB-safe for in-range") {
    let xs = [10, 20, 30];
    assert(xs[0] == 10)?;
    assert(xs[2] == 30)?;
}
