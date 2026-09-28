// A one-level self-unroll must not copy the callee's frame `Seek`: in the
// caller it would move the cursor over the peeled argument's temp slot.
#[max_depth(64)]
fn fibm(int n) -> int {
    if n <= 1 {
        return n;
    }
    return match n {
        default => fibm(n - 1) + fibm(n - 2),
    };
}

fn fib10_plus(int k) -> int {
    let base = k * 2;
    let r = fibm(10);
    return r + base;
}

test("peeled self-call keeps its argument") {
    assert(fib10_plus(3) == 61)?;
}
