// Enums carry a `()` payload past `Result::Ok`: `Option<()>`, a
// `Result` whose `Err` is `()`, and user enums are built, passed,
// returned and matched, and a `()` payload binds (#1449).
use tests::assert;

enum Signal {
    Fire(()),
    Idle,
}

enum Wrap<T> {
    Full(T),
    Empty,
}

fn some_unit() -> Option<()> {
    return Some(());
}

fn mark() {}

fn some_mark() -> Option<()> {
    return Some(mark());
}

fn is_some(Option<()> o) -> bool {
    return match o {
        Some(u) => u == (),
        None => false,
    };
}

fn checked(int n) -> Result<int, ()> {
    if n < 0 {
        return Result::Err(());
    }
    return Result::Ok(n);
}

fn named(string s) -> Result<string, ()> {
    if s == "" {
        return Result::Err(());
    }
    return Result::Ok(s);
}

fn fired(Signal s) -> bool {
    return match s {
        Signal::Fire(u) => u == (),
        Signal::Idle => false,
    };
}

fn full(Wrap<()> w) -> bool {
    return match w {
        Wrap::Full(_) => true,
        Wrap::Empty => false,
    };
}

test("Option<()> is built, passed and matched") {
    assert!(is_some(some_unit()));
    assert!(is_some(some_mark()));
    assert!(!is_some(None));
    let o: Option<()> = Some(());
    assert!(is_some(o));
    let n: Option<()> = None;
    assert!(!is_some(n));
}

test("Option<()> binds its payload") {
    let o = some_unit();
    let seen = match o {
        Some(u) => {
            let v = u;
            v == ()
        },
        None => false,
    };
    assert!(seen);
}

test("a Result with a unit error") {
    assert!(checked(3) == Result::Ok(3));
    assert!(
        match checked(-1) {
            Result::Ok(_) => false,
            Result::Err(e) => e == (),
        },
    );
    assert!(
        match named("a") {
            Result::Ok(s) => s == "a",
            Result::Err(_) => false,
        },
    );
    assert!(match named("") {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    });
}

test("user enums with a unit payload") {
    assert!(fired(Signal::Fire(())));
    assert!(fired(Signal::Fire(mark())));
    assert!(!fired(Signal::Idle));
    assert!(full(Wrap::Full(())));
    assert!(!full(Wrap::Empty));
}
