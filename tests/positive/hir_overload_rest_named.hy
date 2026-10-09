// Overloads with a rest parameter or named arguments, named partials, spreads
// into a partial, and generic rest calls: all lowered by the HIR.
use string::format;

fn f(int x) -> int {
    return x * 10;
}

fn f(int x, int y, int... xs) -> int {
    return x + y + len(xs);
}

fn greet(string name) -> string {
    return name;
}

fn greet(string name, int age) -> string {
    return format("%s:%i", name, age);
}

test("rest and named overloads") {
    assert(f(1) == 10)?;
    assert(f(1, 2) == 3)?;
    assert(f(1, 2, 3, 4) == 5)?;
    assert(greet(name: "Ada") == "Ada")?;
    assert(greet(name: "Grace", age: 40) == "Grace:40")?;
    assert(f(y: 5, x: 1) == 6)?;
}

fn add(int a, int b) -> int {
    return a + b;
}

fn add3(int a, int b, int c) -> int {
    return a + b + c;
}

test("named partial and spreads") {
    let g = add(a: 1);
    assert(g(2) == 3)?;
    let p = add3(1);
    assert(p(...(2, 3)) == 6)?;
    let t = (4, 5);
    assert(p(...t) == 10)?;
}

fn twice_first<T: Num>(T... xs) -> T {
    return xs[0] + xs[0];
}

test("generic rest") {
    assert(twice_first(21) == 42)?;
    assert(twice_first(1.5, 2.0) == 3.0)?;
}
