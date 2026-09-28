// A loop bound folded into a constant can end up as the left operand of the
// loop test; dense emit must rebuild it rather than load an unset register.
fn range_sum() -> int {
    let s = 0;
    for x in 0..3 {
        s = s + x;
    }
    let a = [s, s];
    return a[1];
}

test("constant on the left of a dense loop test") {
    assert(range_sum() == 3)?;
}
