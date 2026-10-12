// A tail call whose callee returns a two-word value (an `Option<int>`)
// hands that pair straight back; its result is never a boxed word.
#[max_depth(64)]
fn find_from(int n, int acc) -> Option<int> {
    let i = 0;
    while i < 3 {
        acc = acc + i;
        i = i + 1;
    }
    if n <= 0 {
        return Option::Some(acc);
    }
    return find_from(n - 1, acc);
}

fn unwrap_or(Option<int> o, int d) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => d,
    };
}

test("tail call returns the callee's pair") {
    assert(unwrap_or(find_from(4, 1), 0) == 16)?;
    assert(unwrap_or(find_from(0, 0), -1) == 3)?;
}
