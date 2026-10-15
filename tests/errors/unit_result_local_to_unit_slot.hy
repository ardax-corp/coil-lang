// #1085: `Ok(())` bound to a local is typed `Result<(), E>`; passing it
// where `Result<unit, E>` is expected converts between the two layouts.
use tests::assert;

fn take(Result<unit, string> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
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
