// S2b: live heap in a mapped frame slot survives alloc + collect.
use gc::{collect};

fn keep([int] xs) -> [int] {
    let junk = [1, 2, 3];
    collect();
    if junk == xs {
        return junk;
    }
    return xs;
}

test("mapped slot survives collect") {
    let a = [42, 7];
    let b = keep(a);
    assert(b[0] == 42)?;
    assert(b[1] == 7)?;
}
