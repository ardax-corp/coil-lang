// Typed inlining takes callees whose fixed-array or tuple parameters are
// only read from, and callees returning a pointer-niche Option. A callee
// that writes through its array parameter keeps its call, so the caller
// still sees the write.

fn take([int] xs) -> int {
    return xs[0] + xs[1];
}

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + take([i, i + 1]);
        i = i + 1;
    }
    return s;
}

fn first((int, int) p) -> int {
    return p[0];
}

fn bump([int] xs) -> int {
    xs[0] = xs[0] + 1;
    return xs[0];
}

fn shared_write() -> int {
    let xs = [1, 2];
    let a = bump(xs);
    return a * 10 + xs[0];
}

fn read_after_copy() -> int {
    let xs = [3, 4];
    let total = take(xs);
    xs[0] = 100;
    return total + xs[0];
}

fn maybe_text(int value) -> Option<string> {
    if value % 3 == 0 {
        return Option::Some("hit");
    }
    return Option::None;
}

fn hits(int n) -> int {
    let i = 0;
    let total = 0;
    while i < n {
        total = total + match maybe_text(i) {
            Option::Some(_) => 1,
            Option::None => 0,
        };
        i = i + 1;
    }
    return total;
}

test("read-only value parameters inline") {
    assert(pack(10) == 100)?;
    assert(first((7, 8)) == 7)?;
    assert(read_after_copy() == 107)?;
}

test("a write through an array parameter reaches the caller") {
    assert(shared_write() == 22)?;
}

test("pointer-niche option returns inline") {
    assert(hits(10) == 4)?;
}
