// `<`, `<=`, `>`, `>=` on tuples order lexicographically: the first
// differing element decides, element by element in its own order, when
// every element type is `Ord`; and such a tuple is `Ord`, so generic code
// bounded by `Ord` takes it (#1721).
use tests::assert;
use collections::sort;

#[derive(Eq, Ord)]
enum Suit {
    Clubs,
    Hearts,
}

fn smaller<T: Ord>(T a, T b) -> T {
    if a < b {
        return a;
    }
    return b;
}

fn ordered<T: Ord>(T a, T b) -> bool {
    return a <= b && b >= a && !(a > b);
}

fn pair(int a, string b) -> (int, string) {
    return (a, b);
}

test("operators") {
    assert!((1, 2) < (1, 3));
    assert!((1, 3) > (1, 2));
    assert!((0, 9) < (1, 0));
    assert!((1, 2) <= (1, 2));
    assert!((1, 2) >= (1, 2));
    assert!(!((1, 2) < (1, 2)));
    assert!(!((2, 0) <= (1, 9)));
}

test("mixed element types") {
    assert!((1, "b") < (2, "a"));
    assert!((1, "a") < (1, "b"));
    assert!((1.5, "z") < (2.5, "a"));
    assert!(pair(3, "x") > pair(3, "w"));
    assert!((1, Suit::Clubs) < (1, Suit::Hearts));
}

test("one-tuples and nested tuples") {
    assert!((1,) < (2,));
    assert!(((1, 2), 3) < ((1, 3), 0));
    assert!((1, (2, "b")) > (1, (2, "a")));
}

test("generic Ord bound") {
    assert!(smaller((2, "a"), (1, "z")) == (1, "z"));
    assert!(ordered((1, "a"), (1, "b")));
    assert!(!ordered((1, "b"), (1, "a")));
}

test("sort a Vec of tuples") {
    let v: Vec<(int, string)> = Vec::new();
    v.push((2, "b"));
    v.push((1, "z"));
    v.push((2, "a"));
    v.push((1, "a"));
    let s = sort(v);
    assert!(s[0] == (1, "a"));
    assert!(s[1] == (1, "z"));
    assert!(s[2] == (2, "a"));
    assert!(s[3] == (2, "b"));
}
