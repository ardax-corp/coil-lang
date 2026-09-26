// A heap-free function (`depth`) gets a precise frame map, so collections
// that run in its callees skip its frame. Heap values held by the callers
// and callees around it must still survive.
use gc::{collect};

class Box {
    pub v: int,
}

fn churn(int n) -> int {
    let keep = new Box(n);
    let i = 0;
    while i < 50 {
        let junk = new Box(i);
        i = i + junk.v - junk.v + 1;
    }
    collect();
    return keep.v;
}

fn depth(int n) -> int {
    if n == 0 {
        return churn(7);
    }
    return depth(n - 1) + 1;
}

test("heap values around a precise frame survive collections") {
    let outer = new Box(40);
    let words = ["a", "b", "c"];
    let total = 0;
    let round = 0;
    while round < 20 {
        total = total + depth(5);
        round = round + 1;
    }
    assert(total == 20 * 12)?;
    assert(outer.v == 40)?;
    assert(words[2] == "c")?;
}
