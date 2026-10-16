// Helper for tests/modules/module_resolution.hy: a function whose body and
// contract call a function declared after it.

fn four() -> int
    ensures result == twice(2)
{
    return twice(2);
}

fn twice(int x) -> int {
    return x * 2;
}
