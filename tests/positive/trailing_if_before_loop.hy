// COI-407: trailing `if` end-label must stay with this function when the
// next body is MIR-dense (HTTP Client::get neighbored numeric helpers).
fn pred(int x) -> int {
    if x == 0 {
        return 1;
    }
    return 2;
}

fn hot(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}

test("trailing if then counted loop") {
    assert(pred(0) == 1)?;
    assert(pred(3) == 2)?;
    assert(hot(5) == 10)?;
}
