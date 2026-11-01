// A readonly class instance's fields never change after `new` (#1560),
// so what is known of them survives calls and loops. A plain class's
// fields do not.

readonly class Bounds {
    pub lo: int,
    pub hi: int,
}

readonly class Buf {
    pub items: Vec<int>,
}

class Box {
    pub n: int,
}

// May write the heap: it appends to `out`.
fn note(Vec<int> out, int x) {
    out.push(x);
}

fn reset(Box b) {
    b.n = -1;
}

fn span(Bounds r) -> int
    requires r.lo >= 0 && r.lo <= r.hi
    ensures result >= 0
{
    return r.hi - r.lo;
}

fn width_after_note(Bounds r, Vec<int> out) -> int
    requires r.lo >= 0 && r.lo <= r.hi
    ensures result >= 0
{
    note(out, r.lo);
    return r.hi - r.lo;
}

fn span_after_note(Bounds r, Vec<int> out) -> int {
    note(out, r.hi);
    if r.lo >= 0 && r.lo <= r.hi {
        out.push(r.lo);
        return span(r);
    }
    return 0;
}

fn fill(Bounds r, Vec<int> out) -> int
    requires r.lo >= 0 && r.lo <= r.hi && r.hi < 1000
    ensures result == r.hi - r.lo
{
    let i = r.lo;
    let k = 0;
    while i < r.hi
        invariant r.lo <= i && i <= r.hi && k == i - r.lo
    {
        note(out, i);
        i = i + 1;
        k = k + 1;
    }
    return k;
}

// `reset` writes `b.n`: nothing is known of it afterwards.
fn box_after_reset(Box b) -> int
    requires b.n >= 0 && b.n < 1000
    ensures result >= 0
{
    reset(b);
    return b.n;
}

// A plain class field, and a call that may write it.
fn box_after_note(Box b, Vec<int> out) -> int
    requires b.n >= 0 && b.n < 1000
    ensures result >= 0
{
    note(out, b.n);
    return b.n;
}

// A `readonly` view promises nothing: `b` may be `v`.
fn view_after_reset(readonly Box v, Box b) -> int
    requires v.n >= 0 && v.n < 1000
    ensures result >= 0
{
    reset(b);
    return v.n;
}

// The `items` slot never changes, but the vector it holds may be `out`.
fn items_after_note(Buf b, Vec<int> out) -> int
    requires b.items.len() > 0
    ensures result > 0
{
    note(out, 0);
    return b.items.len();
}
