// Every clause here holds for every input.

fn clamp(int x, int lo, int hi) -> int
    requires lo <= hi
    ensures result >= lo && result <= hi
{
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn abs_small(int x) -> int
    requires x > -1000 && x < 1000
    ensures result >= 0 && (result == x || result == -x)
{
    if x < 0 {
        return -x;
    }
    return x;
}

fn clamp_twice(int x) -> int
    ensures result >= 0 && result <= 10
{
    return clamp(clamp(x, 0, 100), 0, 10);
}

fn last(Vec<int> v) -> int
    requires len(v) > 0
{
    return v[len(v) - 1];
}

fn push_then_last(Vec<int> v, int x) -> int {
    v.push(x);
    return last(v);
}

fn count_up(int n) -> int
    requires n >= 0 && n < 1000000
    ensures result == n
{
    let i = 0;
    while i < n
        invariant i >= 0 && i <= n
    {
        i = i + 1;
    }
    return i;
}
