// `int as bool` is `n != 0` and `bool as int` is `0` / `1`, on runtime
// values in direct MIR as in the VM's casts.
use tests::assert;

fn truthy(int n) -> bool {
    return n as bool;
}

fn count(bool a, bool b, bool c) -> int {
    return (a as int) + (b as int) + (c as int);
}

test("int and bool casts on runtime values") {
    assert!(truthy(0) == false);
    assert!(truthy(1));
    assert!(truthy(-7));
    assert!(truthy(1 << 40));
    assert!(count(true, false, true) == 2);
    assert!(count(truthy(3), truthy(0), truthy(-1)) == 2);
}
