// True, but the loop invariant is too weak to show it.

fn sum_to(int n) -> int
    requires n >= 0 && n < 1000
    ensures result >= 0
{
    let s = 0;
    let i = 0;
    while i < n
        invariant s >= 0
    {
        i = i + 1;
        s = s + i;
    }
    return s;
}
