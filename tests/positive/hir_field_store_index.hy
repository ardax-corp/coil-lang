// A field store whose base indexes a Vec, an array or a tuple: the value,
// then the base, then `SetField`, as the AST.

class B {
    pub v: int,
}

class A {
    pub b: B,
}

test("through a Vec element") {
    let xs: Vec<B> = Vec::from([new B(1), new B(2)]);
    xs[0].v = 9;
    assert(xs[0].v + xs[1].v == 11)?;
}

test("through a nested field") {
    let xs: Vec<A> = Vec::from([new A(new B(1)), new A(new B(2))]);
    let i = 1;
    xs[i].b.v = 7;
    assert(xs[0].b.v * 10 + xs[1].b.v == 17)?;
}

test("compound store") {
    let xs: Vec<B> = Vec::from([new B(4)]);
    xs[0].v += 3;
    xs[0].v *= 2;
    assert(xs[0].v == 14)?;
}

test("through a tuple") {
    let t = (new B(1), new B(2));
    t[1].v = 5;
    assert(t[0].v + t[1].v == 6)?;
}
