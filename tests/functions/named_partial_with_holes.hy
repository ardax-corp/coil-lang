// Named partial application may leave holes before or between the named
// arguments; the holes fill in declaration order (coil-lang#1156).
use tests::assert;

fn f3(int a, int b, int c) -> int {
    return a * 100 + b * 10 + c;
}

fn label(string pre, int n, string post) -> string {
    return pre + post;
}

test("hole before a named argument") {
    let p = f3(b: 2);
    assert!(p(1, 3) == 123);
}

test("only the last parameter named") {
    let q = f3(c: 9);
    assert!(q(1, 2) == 129);
}

test("first and last named, middle hole") {
    let q = f3(a: 7, c: 9);
    assert!(q(8) == 789);
}

test("named hole partial on mixed types") {
    let l = label(post: "!");
    assert!(l("hi", 3) == "hi!");
}

test("named partial argument is evaluated once, at the partial") {
    let k = 4;
    let p = f3(b: k);
    k = 5;
    assert!(p(1, 0) == 140);
}
