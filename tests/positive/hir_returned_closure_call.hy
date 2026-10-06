// A closure returned by a named, non-generic function lowers from HIR when
// a local bound to it is called: the call returns one function word, then
// `CallIndirect` through the local.
fn adder(int k) -> fn(int x) -> int {
    return fn (int y) use (k) => y + k;
}

fn mixer(int a, int b) -> fn(int x, int y) -> int {
    return fn (int x, int y) use (a, b) => x * a + y * b;
}

fn greeter(string hello) -> fn(string who) -> string {
    return fn (string who) use (hello) => hello + " " + who;
}

test("call a returned closure") {
    let add3 = adder(3);
    assert(add3(4) == 7)?;
    assert(add3(add3(1)) == 7)?;
}

test("returned closure with two params") {
    let m = mixer(2, 10);
    assert(m(1, 2) == 22)?;
}

test("returned closure over a string") {
    let g = greeter("hi");
    assert(g("coil") == "hi coil")?;
}
