// A `return match` whose every arm is a self call becomes tail calls, so
// deep bouncing recursion runs in constant frames under both codegens.
fn bounce(Option<int> o, int n) -> int {
    if n == 0 {
        return 7;
    }
    return match o {
        Option::None => bounce(Option::Some(0), n - 1),
        Option::Some(_) => bounce(Option::None, n - 1),
    };
}

test("tail match self calls run deep") {
    assert(bounce(Option::None, 200000) == 7)?;
}
