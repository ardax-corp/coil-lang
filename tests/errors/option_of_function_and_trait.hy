// `Option` / `Result` holding a function value or a trait value, as a
// local, a parameter and a class field (#1991).
use tests::assert;

trait Named<T> {
    fn name(T x) -> string {}
}

class Dog {
    pub n: string,
}

impl Named for Dog {
    fn name(Dog d) -> string {
        return d.n;
    }
}

class Handler {
    pub f: Option<fn() -> string>,
}

fn call(Handler h) -> string {
    return match h.f {
        Option::Some(g) => g(),
        Option::None => "none",
    };
}

fn apply(Option<fn(int) -> int> f, int x) -> int {
    return match f {
        Option::Some(g) => g(x),
        Option::None => x,
    };
}

fn pick(bool ok) -> Result<fn(int) -> int, string> {
    if ok {
        return Result::Ok(fn (int x) => x * 2);
    }
    return Result::Err("no");
}

test("an optional function field") {
    let h = new Handler(Option::Some(fn () => "hi"));
    let empty = new Handler(Option::None);
    assert!(call(h) == "hi");
    assert!(call(empty) == "none");
}

test("an optional function parameter and a capturing closure") {
    let k = 10;
    assert!(apply(Option::Some(fn (int x) use (k) => x + k), 1) == 11);
    assert!(apply(Option::None, 1) == 1);
}

test("a Result holding a function") {
    let doubled = match pick(true) {
        Result::Ok(f) => f(4),
        Result::Err(_) => -1,
    };
    assert!(doubled == 8);
    let failed = match pick(false) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    assert!(failed == "no");
}

test("an optional trait value") {
    let some: Option<Named> = Option::Some(new Dog("rex"));
    let got = match some {
        Option::Some(d) => d.name(),
        Option::None => "",
    };
    assert!(got == "rex");
}
