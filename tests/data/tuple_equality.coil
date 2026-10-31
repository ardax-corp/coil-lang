// `==` / `!=` on tuples is structural: element-wise and recursive, over
// mixed element types, one-tuples, nested tuples and arrays inside.
use tests::assert;

fn pair(int a, string b) -> (int, string) {
    return (a, b);
}

test("flat tuples") {
    assert!((1, "a") == (1, "a"));
    assert!((1, "a") != (1, "b"));
    assert!((1, "a") != (2, "a"));
    assert!((1.5, true) == (1.5, true));
    assert!((1.5, true) != (1.5, false));
}

test("one-tuples") {
    assert!((5,) == (5,));
    assert!((5,) != (6,));
}

test("nested tuples and arrays") {
    assert!((1, (2, 3)) == (1, (2, 3)));
    assert!((1, (2, 3)) != (1, (2, 4)));
    let a = (1, [1, 2]);
    let b = (1, [1, 2]);
    assert!(a == b);
    assert!(a != (1, [1, 3]));
}

test("built at runtime compare by content") {
    let s = "a" + "b";
    assert!(pair(1, s) == (1, "ab"));
    assert!(pair(1, s) == pair(1, "ab"));
    assert!(pair(2, s) != pair(1, s));
}
