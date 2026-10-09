// Inner-pattern dispatch + nested record patterns.
enum Opt {
    Nada,
    Yea(int),
}

enum Res {
    Good(Opt),
    Bad(string),
}

enum Inner {
    I { v: int },
}

enum Wrap {
    W { inner: Inner, name: string },
}

enum Inner2 {
    I2 { x: int, y: int },
}

enum Wrap2 {
    W2 { inner: Inner2, name: int },
}

fn unwrap_res(Res r) -> int {
    return match r {
        Res::Good(Opt::Yea(v)) => v,
        Res::Good(Opt::Nada) => 0,
        Res::Bad(_) => -1,
    };
}

fn get_v(Wrap w) -> int {
    return match w {
        Wrap::W { inner: Inner::I { v }, name } => v,
    };
}

fn both(Wrap2 w) -> int {
    return match w {
        Wrap2::W2 { inner: Inner2::I2 { x, y }, name } => x + y + name,
    };
}

test("inner pattern some") {
    assert(unwrap_res(Res::Good(Opt::Yea(42))) == 42)?;
}

test("inner pattern none") {
    assert(unwrap_res(Res::Good(Opt::Nada)) == 0)?;
}

test("inner pattern err") {
    assert(unwrap_res(Res::Bad("x")) == -1)?;
}

test("nested record pattern") {
    let w = Wrap::W { inner: Inner::I { v: 99 }, name: "x" };
    assert(get_v(w) == 99)?;
}

test("nested multifield record preserves sibling") {
    let w = Wrap2::W2 { inner: Inner2::I2 { x: 10, y: 20 }, name: 3 };
    assert(both(w) == 33)?;
}
