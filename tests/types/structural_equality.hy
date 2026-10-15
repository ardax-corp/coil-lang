// `==` / `!=` on arrays and tuples are structural (length and each element,
// recursively); strings compare by content, including strings built at run
// time.
use tests::assert;

fn build(string a, string b) -> string {
    return a + b;
}

test("tuples") {
    assert!((1, "a") == (1, "a"));
    assert!((1, "a") != (1, "b"));
    assert!(((1, 2), 3) == ((1, 2), 3));
    assert!(((1, 2), 3) != ((1, 3), 3));
}

test("arrays") {
    assert!([1, 2, 3] == [1, 2, 3]);
    assert!([1, 2, 3] != [1, 2, 4]);
    assert!([[1], [2]] == [[1], [2]]);
    let v: Vec<int> = [1, 2];
    let w: Vec<int> = [1, 2];
    assert!(v == w);
    w.push(3);
    assert!(v != w);
}

test("strings by content") {
    let s = build("ab", "c");
    assert!(s == "abc");
    assert!(s != "abd");
    assert!(build("", "") == "");
    assert!(build("a", "") == "a");
    assert!(build("é", "") == "é");
}

test("bools and ints") {
    assert!(true == true);
    assert!(true != false);
    assert!((1 < 2) == (3 < 4));
    assert!(0 == -0);
}
