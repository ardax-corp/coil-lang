// A generic call whose argument and result is an `Option` / `Result` with a
// type variable nothing pins (`id(Ok(9))`: the error type is never named)
// passes the enum's boxed word, as a plain `let r = Ok(9);` keeps it,
// called by name or through a `PolyFn` value (#1517).
use tests::assert;

fn id<T>(T x) -> T {
    return x;
}

fn first<T>(T a, T b) -> T {
    return a;
}

fn tag<U>(U u) -> int {
    let r = id(Ok(u));
    return match r {
        Ok(_) => 1,
        Err(_) => 0,
    };
}

test("open error type") {
    let r = id(Ok(9));
    let b = match r {
        Ok(x) => x,
        Err(_) => -1,
    };
    assert!(b == 9);
}

test("open ok type") {
    let r = id(Err("no"));
    let b = match r {
        Ok(_) => "",
        Err(e) => e,
    };
    assert!(b == "no");
}

test("open option payload") {
    let r = id(None);
    let b = match r {
        Some(_) => -1,
        None => 3,
    };
    assert!(b == 3);
}

test("two open arguments") {
    let r = first(Ok(4), Ok(5));
    let b = match r {
        Ok(x) => x,
        Err(_) => -1,
    };
    assert!(b == 4);
}

test("through a function value") {
    let i = id;
    let r = i(Ok(9));
    let b = match r {
        Ok(x) => x,
        Err(_) => -1,
    };
    assert!(b == 9);
    let n = i(None);
    let c = match n {
        Some(_) => -1,
        None => 2,
    };
    assert!(c == 2);
}

test("inside a generic body") {
    assert!(tag(5) == 1);
    assert!(tag("s") == 1);
}
