// S2b: live heap in a mapped frame slot survives alloc + collect.
// Fixed-length array params and returns keep `keep` out of line.
use gc::collect;

fn keep([int; 2] xs) -> [int; 2] {
    let junk = [1, 2];
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
