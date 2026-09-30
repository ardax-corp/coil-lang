// A frame-local `let` of a niche-layout enum (`Option<heap>`,
// `Result<(), heap>`, `Result<heap, heap>`) is one word, whichever way the
// constructor is spelled. Binding it as `[payload, tag]` underflowed the
// stack on the following `match`.
class C {
    pub v: int,
}

class E {
    pub code: int,
}

fn make_c(int v) -> C {
    return new C(v);
}

fn opt_v(Option<C> o) -> int {
    return match o {
        Option::Some(c) => c.v,
        Option::None => -1,
    };
}

test("Option::Some of a call result") {
    let o: Option<C> = Option::Some(make_c(7));
    let r = match o {
        Option::Some(c) => c.v,
        Option::None => 0,
    };
    assert(r == 7)?;
}

test("Option::Some of a local") {
    let c = make_c(3);
    let o: Option<C> = Option::Some(c);
    let r = match o {
        Option::Some(x) => x.v,
        Option::None => 0,
    };
    assert(r == 3)?;
    assert(opt_v(o) == 3)?;
}

test("Option::None stays a niche") {
    let o: Option<C> = Option::None;
    let r = match o {
        Option::Some(x) => x.v,
        Option::None => 11,
    };
    assert(r == 11)?;
}

test("unit Result niche") {
    let ok: Result<(), E> = Result::Ok(());
    let err: Result<(), E> = Result::Err(new E(5));
    let a = match ok {
        Result::Ok(_) => 1,
        Result::Err(e) => e.code,
    };
    let b = match err {
        Result::Ok(_) => 1,
        Result::Err(e) => e.code,
    };
    assert(a == 1)?;
    assert(b == 5)?;
}

test("heap Result niche") {
    let ok: Result<C, E> = Result::Ok(make_c(9));
    let err: Result<C, E> = Result::Err(new E(4));
    let a = match ok {
        Result::Ok(c) => c.v,
        Result::Err(e) => e.code,
    };
    let b = match err {
        Result::Ok(c) => c.v,
        Result::Err(e) => e.code,
    };
    assert(a == 9)?;
    assert(b == 4)?;
}

test("immediate Option stays two-slot") {
    let o: Option<int> = Option::Some(make_c(6).v);
    let r = match o {
        Option::Some(n) => n,
        Option::None => 0,
    };
    assert(r == 6)?;
}
