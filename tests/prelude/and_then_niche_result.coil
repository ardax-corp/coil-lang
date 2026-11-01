// `and_then` whose function changes the `Ok` type to a niche layout: the
// shared body gets the function's result as its boxed enum.
use tests::assert;

enum E {
    A,
    B,
}

class T {
    pub n: int,
}

fn mk(int n) -> Result<T, E> {
    return new T(n);
}

fn show(T t) -> Result<string, E> {
    return "x";
}

fn label(int x) -> Result<string, E> {
    if x > 0 {
        return "pos";
    }
    raise E::A;
}

fn fail(T t) -> Result<string, E> {
    raise E::B;
}

fn some_name(int x) -> Option<string> {
    return Some("n");
}

test("class Ok to string Ok") {
    let r = mk(1).and_then(fn (T t) => show(t));
    assert!(r.is_ok(), "lambda");
    assert!(mk(1).and_then(show).unwrap_or("") == "x", "named fn");
    if r.is_err() {
        panic "Err";
    }
}

test("int Ok to string Ok") {
    let o: Result<int, E> = Ok(3);
    assert!(o.and_then(label).unwrap_or("") == "pos", "pos");
    let n: Result<int, E> = Ok(0);
    assert!(n.and_then(label).is_err(), "err from the function");
    assert!(o.and_then(fn (int x) => label(x)).unwrap_or("") == "pos", "lambda");
}

test("Err from the function and from the receiver") {
    assert!(mk(1).and_then(fail).is_err(), "function Err");
    let e: Result<T, E> = Err(E::A);
    assert!(e.and_then(show).is_err(), "receiver Err");
}

test("built string survives") {
    let s = mk(1).and_then(fn (T t) => Result::Ok("x" + "y")).unwrap_or("");
    assert!(s == "xy", "concat");
}

test("Option and_then to a niche Option") {
    let o: Option<int> = Some(2);
    assert!(o.and_then(some_name).unwrap_or("") == "n", "named fn");
}

fn small(int x) -> Result<(), string> {
    if x > 5 {
        raise "big";
    }
    return;
}

test("unit Ok from the function") {
    let a: Result<int, string> = Ok(3);
    assert!(a.and_then(small).is_ok(), "ok");
    let b: Result<int, string> = Ok(9);
    assert!(b.and_then(small).is_err(), "err");
}
