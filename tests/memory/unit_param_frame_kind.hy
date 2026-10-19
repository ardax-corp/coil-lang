// A `unit` parameter holds the empty tuple object, a heap word: it stays
// valid across collections in the callee's frame and when passed on (#1465).
use tests::assert;
use string::format;

fn churn(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        let s = format("%i-%s", i, "xyzzy");
        total = total + s.len();
        i = i + 1;
    }
    return total;
}

fn shows_unit(unit u) -> bool {
    return format("%v", u) == "()";
}

fn hold(unit u, int n) -> int {
    let k = churn(n);
    if shows_unit(u) {
        return k;
    }
    return -1;
}

fn pass_on(unit u, int n) -> bool {
    let k = churn(n);
    return shows_unit(u) && k > 0;
}

test("a unit parameter survives collections") {
    let i = 0;
    while i < 20 {
        assert!(hold((), 50) > 0);
        assert!(pass_on((), 50));
        i = i + 1;
    }
}
