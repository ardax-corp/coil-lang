// `?` on a self-recursive call in a function declared `-> Option<T>` is an
// Option `?` (coil-lang#1158).
use tests::assert;

fn halvings(int n) -> Option<int> {
    if n == 1 {
        return Option::Some(0);
    }
    if n % 2 == 1 {
        return Option::None;
    }
    let rest = halvings(n / 2)?;
    return Option::Some(rest + 1);
}

test("option try on a self call propagates Some") {
    assert!(halvings(8) == Option::Some(3));
    assert!(halvings(1) == Option::Some(0));
}

test("option try on a self call propagates None") {
    assert!(halvings(12) == Option::None);
}
