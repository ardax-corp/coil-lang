// A helper with no declared return type that uses `assert!` infers
// `Result<unit, string>`: a failed assert returns `Err` from the helper,
// not from the test. Calling it as a bare statement drops that `Err`, so
// the case goes on; `helper()?;` hands it to the caller (#1815).
use tests::assert;

fn failing() {
    assert!(1 == 2, "inner");
}

fn passing() {
    assert!(1 == 1, "inner");
}

fn dropped() -> Result<unit, string> {
    let _ = failing();
    return Result::Ok(());
}

fn propagated() -> Result<unit, string> {
    failing()?;
    return Result::Ok(());
}

fn propagated_ok() -> Result<unit, string> {
    passing()?;
    return Result::Ok(());
}

fn verdict(Result<unit, string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

test("a discarded helper Err does not fail its caller") {
    assert!(verdict(dropped()) == "ok");
}

test("a helper's failed assert propagates through ?") {
    assert!(verdict(propagated()) == "inner");
    assert!(verdict(failing()) == "inner");
}

test("a helper's passing assert propagates Ok through ?") {
    assert!(verdict(propagated_ok()) == "ok");
    passing()?;
}
