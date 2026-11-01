// A bare case name in a pattern takes the scrutinee's own enum first
// (#1352): `Ok` / `Err` stay unambiguous on a `Result` even though `Reply`
// declares the same names, and on a `Reply` they name `Reply`'s cases.
use tests::assert;

enum Reply {
    Ok,
    Err(string),
    Later { secs: int },
}

fn from_result(Result<int, string> r) -> int {
    return match r {
        Ok(n) => n,
        Err(_) => -1,
    };
}

fn from_reply(Reply r) -> int {
    return match r {
        Ok => 1,
        Err(_) => 2,
        Later { secs } => secs,
    };
}

fn is_ok(Reply r) -> bool {
    if let Ok = r {
        return true;
    }
    return false;
}

test("the scrutinee's enum resolves a shared case name") {
    assert!(from_result(Result::Ok(7)) == 7);
    assert!(from_result(Result::Err("e")) == -1);
    assert!(from_reply(Reply::Ok) == 1);
    assert!(from_reply(Reply::Err("x")) == 2);
    assert!(from_reply(Reply::Later { secs: 30 }) == 30);
    assert!(is_ok(Reply::Ok));
    assert!(!is_ok(Reply::Later { secs: 1 }));
}
