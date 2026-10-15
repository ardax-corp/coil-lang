// #1085: `Ok(())` bound to a local is typed `Result<(), E>`; passing it
// where `Result<unit, E>` is expected converts between the two layouts.
use tests::assert;

fn take(Result<unit, string> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

fn message(Result<unit, string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(m) => m,
    };
}

fn fail() -> Result<(), string> {
    return Result::Err("boom");
}

fn give() -> Result<unit, string> {
    let r = Result::Ok(());
    return r;
}

test("let-bound Ok(()) pushed into Vec<Result<unit, string>>") {
    let v: Vec<Result<unit, string>> = [];
    let r = Result::Ok(());
    v.push(r);
    let e = Result::Err("bad");
    v.push(e);
    assert!(len(v) == 2);
    assert!(take(v[0]));
    assert!(!take(v[1]));
}

test("let-bound Ok(()) passed to a Result<unit, string> parameter") {
    let r = Result::Ok(());
    assert!(take(r));
}

test("let-bound Ok(()) returned as Result<unit, string>") {
    assert!(take(give()));
}

test("a Result<(), string> Err keeps its payload in a Result<unit, string> slot") {
    let r = fail();
    assert!(message(r) == "boom");
    let v: Vec<Result<unit, string>> = [];
    v.push(fail());
    let ok = Result::Ok(());
    v.push(ok);
    assert!(message(v[0]) == "boom");
    assert!(message(v[1]) == "ok");
}
