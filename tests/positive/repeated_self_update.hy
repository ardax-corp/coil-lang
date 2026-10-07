// `n = n + 1` twice in one block: the second add reads the updated `n`
// (local CSE must not reuse the first sum).

fn three() -> int {
    let n = 0;
    n = n + 1;
    n = n + 1;
    n = n + 1;
    return n;
}

fn interleaved() -> int {
    let n = 0;
    let i = 0;
    n = n + 2;
    i = 1;
    n = n + 2;
    i = 2;
    return n + i;
}

test("repeated self increments") {
    assert(three() == 3)?;
}

test("increments with other stores between") {
    assert(interleaved() == 6)?;
}
