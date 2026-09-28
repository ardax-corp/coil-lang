// Two self-call results added inside a match arm: the sum flows to a join,
// so it is stored while its operands are still on the stack.
#[max_depth(64)]
fn fibm(int n) -> int {
    if n <= 1 {
        return n;
    }
    return match n {
        default => fibm(n - 1) + fibm(n - 2),
    };
}

test("self-call results summed into a match join") {
    assert(fibm(15) == 610)?;
}
