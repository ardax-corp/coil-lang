// A named function read as a value is `MakeFn` over its entry, as the
// AST does; the callee is then called through `CallIndirect`.
fn double(int x) -> int {
    return x * 2;
}

fn seven() -> int {
    return 7;
}

fn apply(int -> int f, int x) -> int {
    return f(x);
}

fn twice(int x) -> int {
    let f = double;
    return apply(f, apply(double, x));
}

fn pick(bool d) -> int {
    let g = seven;
    if d {
        return apply(double, g());
    }
    return g();
}

test("named functions as values") {
    assert(twice(3) == 12)?;
    assert(pick(true) == 14)?;
    assert(pick(false) == 7)?;
}
