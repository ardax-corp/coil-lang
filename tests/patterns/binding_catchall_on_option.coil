// A bare binding as the last arm of a match on an `Option` binds the
// whole value (#1161).
use tests::assert;

fn first_or(Option<int> o, int fallback) -> int {
    return match o {
        Option::Some(v) => v,
        rest => fallback,
    };
}

fn first_or_str(Option<string> o, string fallback) -> string {
    return match o {
        Option::Some(v) => v,
        rest => fallback,
    };
}

fn is_none_by_rest(Option<int> o) -> bool {
    return match o {
        Option::Some(v) => false,
        rest => match rest {
            Option::None => true,
            default => false,
        },
    };
}

test("a binding arm on Option<int> takes the rest") {
    assert!(first_or(Option::None, 9) == 9);
    assert!(first_or(Option::Some(4), 9) == 4);
}

test("a binding arm on Option<string> takes the rest") {
    assert!(first_or_str(Option::None, "x") == "x");
    assert!(first_or_str(Option::Some("a"), "x") == "a");
}

test("the bound value is the whole scrutinee") {
    assert!(is_none_by_rest(Option::None));
    assert!(!is_none_by_rest(Option::Some(1)));
}
