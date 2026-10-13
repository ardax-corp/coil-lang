// `==` / `!=` on niche `Option` / `Result` words compares the boxed
// values structurally, as the VM's `EQ`: the variant, then the payload
// (a string by content).
use tests::assert;

fn wrap(string s, bool ok) -> Result<string, string> {
    if ok {
        return Result::Ok(s + "!");
    }
    return Result::Err(s + "?");
}

fn some(string s, bool on) -> Option<string> {
    if on {
        return Option::Some(s + "!");
    }
    return Option::None;
}

fn unit(bool ok) -> Result<(), string> {
    if ok {
        return Result::Ok(());
    }
    return Result::Err("bad");
}

fn same_result(Result<string, string> a, Result<string, string> b) -> bool {
    return a == b;
}

fn same_option(Option<string> a, Option<string> b) -> bool {
    return a == b;
}

fn same_unit(Result<(), string> a, Result<(), string> b) -> bool {
    return a == b;
}

test("niche words compare structurally") {
    assert!(same_result(wrap("a", true), wrap("a", true)));
    assert!(!same_result(wrap("a", true), wrap("a", false)));
    assert!(!same_result(wrap("a", true), wrap("b", true)));
    assert!(same_result(wrap("e", false), Result::Err("e?")));
    assert!(same_option(some("x", true), some("x", true)));
    assert!(same_option(some("x", false), Option::None));
    assert!(!same_option(some("x", true), Option::None));
    assert!(same_unit(unit(true), unit(true)));
    assert!(same_unit(unit(false), unit(false)));
    assert!(!same_unit(unit(true), unit(false)));
}
