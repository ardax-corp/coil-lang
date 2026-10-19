// A generic returning a tuple of its type parameter, called at a two-word
// type (`Option<int>`): the result is a heap tuple whose items ride boxed,
// and destructuring it unboxes each item (#1486).
use tests::assert;

fn both<T>(T a, T b) -> (T, T) {
    return (a, b);
}

test("a tuple of Option<int> from a generic destructures") {
    let x: Option<int> = Option::Some(5);
    let y: Option<int> = Option::None;
    let (a, b) = both(x, y);
    assert!(a == Option::Some(5));
    assert!(b == Option::None);
}
