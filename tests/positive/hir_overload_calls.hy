// Calls to overloaded functions go to the selected overload's keyed entry,
// by type and by arity.
fn mag(int x) -> int {
    if x < 0 {
        return 0 - x;
    }
    return x;
}

fn mag(float x) -> float {
    if x < 0.0 {
        return 0.0 - x;
    }
    return x;
}

fn join(string a) -> string {
    return a;
}

fn join(string a, string b) -> string {
    return a + b;
}

fn join(string a, string b, string c) -> string {
    return a + b + c;
}

fn spread(int a, int b) -> int {
    return mag(a - b) * 10 + mag(b - a);
}

fn scaled(float x) -> float {
    return mag(x) * 2.0;
}

test("overloads chosen by argument type") {
    assert(mag(-4) == 4)?;
    assert(mag(3) == 3)?;
    assert(spread(2, 7) == 55)?;
    assert(scaled(-1.5) == 3.0)?;
}

test("overloads chosen by arity") {
    assert(join("a") == "a")?;
    assert(join("a", "b") == "ab")?;
    assert(join("a", "b", join("c")) == "abc")?;
}
