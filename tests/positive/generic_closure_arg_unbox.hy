// A ground closure passed where a generic callee takes `T -> U` gets its
// `T` arguments boxed by the generic ABI; it is called with them unboxed
// (#699), whether it is a lambda literal or a named function.
// HIR only: coil-lang#785 (the AST codegen does not carry this fix).
use string::format;

fn apply_to<T, U>(T x, T -> U f) -> U {
    return f(x);
}

fn twice<T>(T x, T -> T f) -> T {
    return f(f(x));
}

fn dbl(int y) -> int {
    return y * 2;
}

test("lambda literal receives an unboxed int") {
    assert(apply_to(21, fn (int y) => y * 2) == 42)?;
}

test("named function receives an unboxed int") {
    assert(apply_to(21, dbl) == 42)?;
}

test("string, float and nested calls") {
    assert(apply_to("ab", fn (string s) => len(s)) == 2)?;
    assert(apply_to(2.5, fn (float y) => y * 2.0) == 5.0)?;
    assert(apply_to(3, fn (int y) => format("%i", y)) == "3")?;
    assert(twice(5, fn (int y) => y + 1) == 7)?;
}
