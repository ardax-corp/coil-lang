// A call resolves against every overload of a name, including the ones
// declared further down the file (#848).

fn f(int x) -> int {
    return x;
}

fn g(string s) -> int {
    return f(s);
}

fn h(int x) -> int {
    return f(x) + f("abc");
}

fn f(string s) -> int {
    return len(s);
}

test("overload declared after its caller") {
    assert(g("ab") == 2)?;
    assert(h(4) == 7)?;
}
