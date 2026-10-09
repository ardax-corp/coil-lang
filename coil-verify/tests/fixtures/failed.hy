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
