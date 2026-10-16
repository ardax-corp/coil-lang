// Option `?` propagates `None`, which carries no payload: the operand's
// `Option<T>` need not have the function's `T` (#1165).
use tests::assert;

fn f(Option<string> s) -> Option<int> {
    let v = s?;
    return Option::Some(len(v));
}

fn flat(Option<Option<int>> b) -> Option<int> {
    let inner = b?;
    return Option::Some(inner ?? 0);
}

fn mixed(Option<int> a, Option<string> s) -> Option<string> {
    let n = a?;
    let t = s?;
    if n > 0 {
        return Option::Some(t);
    }
    return Option::Some("");
}

test("`?` on an Option of another payload type") {
    assert!(f(Option::Some("ab")) == Option::Some(2));
    assert!(f(Option::None) == Option::None);
}

test("`?` on a nested Option, then `??`") {
    assert!(flat(Option::Some(Option::Some(5))) == Option::Some(5));
    assert!(flat(Option::Some(Option::None)) == Option::Some(0));
    assert!(flat(Option::None) == Option::None);
}

test("two `?`s of different payloads") {
    assert!(mixed(Option::Some(3), Option::Some("x")) == Option::Some("x"));
    assert!(mixed(Option::None, Option::Some("x")) == Option::None);
    assert!(mixed(Option::Some(3), Option::None) == Option::None);
}
