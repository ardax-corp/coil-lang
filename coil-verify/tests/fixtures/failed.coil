// Each function breaks its contract for some input.

fn wrong_max(int a, int b) -> int
    ensures result >= a && result >= b
{
    if a > b {
        return b;
    }
    return a;
}

fn half(int x) -> int
    requires x >= 0
{
    return x / 2;
}

fn calls_half(int x) -> int {
    return half(x - 1);
}

class Counter {
    pub n: int,
}

// `q` is `p`, so the write shows through `p.x`.
fn aliased(int a) -> int
    requires a >= 0 && a < 1000
    ensures result == a
{
    let p = { x: a };
    let q = p;
    q.x = q.x + 1;
    return p.x;
}

// `c` and `d` may be one object.
fn maybe_same(Counter c, Counter d) -> int
    requires c.n == 0
    ensures result == 0
{
    d.n = 5;
    return c.n;
}
