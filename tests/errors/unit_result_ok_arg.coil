// `Result::Ok(())` is typed `Result<(), E>`, but stored into a
// `Vec<Result<unit, E>>` (pushed or written by index) or passed to a
// `Result<unit, E>` parameter it must be the unit-result word, so that
// reading it back takes the `Ok` arm.
use tests::assert;

enum E1 {
    Bad,
}

fn is_ok(Result<unit, E1> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        default => false,
    };
}

fn is_bad(Result<unit, E1> r) -> bool {
    return match r {
        Result::Err(E1::Bad) => true,
        default => false,
    };
}

test("Ok(()) pushed into a Vec<Result<unit, E>> reads back as Ok") {
    let v: Vec<Result<unit, E1>> = Vec::new();
    v.push(Result::Ok(()));
    v.push(Result::Err(E1::Bad));
    let ok = match v[0] {
        Result::Ok(_) => true,
        default => false,
    };
    assert!(ok);
    assert!(is_ok(v[0]));
    assert!(is_bad(v[1]));
}

test("Ok(()) stored by index into a Vec<Result<unit, E>> reads back as Ok") {
    let v: Vec<Result<unit, E1>> = Vec::new();
    v.push(Result::Err(E1::Bad));
    v.push(Result::Err(E1::Bad));
    v[0] = Result::Ok(());
    assert!(is_ok(v[0]));
    assert!(is_bad(v[1]));
}

test("Ok(()) passed as a Result<unit, E> argument is Ok") {
    assert!(is_ok(Result::Ok(())));
    assert!(is_bad(Result::Err(E1::Bad)));
}
