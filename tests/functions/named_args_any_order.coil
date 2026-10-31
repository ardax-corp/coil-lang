// Named arguments may come in any order after a positional prefix; each
// lands in its own parameter whatever the order written.
use tests::assert;
use string::format;

fn digits(int a, int b, int c) -> int {
    return a * 100 + b * 10 + c;
}

fn tag(string name, int n, bool loud) -> string {
    let s = format("%s%i", name, n);
    if loud {
        return s + "!";
    }
    return s;
}

test("every order of three named arguments") {
    assert!(digits(a: 1, b: 2, c: 3) == 123);
    assert!(digits(a: 1, c: 3, b: 2) == 123);
    assert!(digits(b: 2, a: 1, c: 3) == 123);
    assert!(digits(b: 2, c: 3, a: 1) == 123);
    assert!(digits(c: 3, a: 1, b: 2) == 123);
    assert!(digits(c: 3, b: 2, a: 1) == 123);
}

test("a positional prefix then shuffled names") {
    assert!(digits(1, c: 3, b: 2) == 123);
    assert!(digits(1, 2, c: 3) == 123);
    assert!(digits(4, b: 5, c: 6) == 456);
}

test("parameters of different types") {
    assert!(tag(loud: true, n: 3, name: "x") == "x3!");
    assert!(tag("y", loud: false, n: 7) == "y7");
    assert!(tag(n: 0, name: "z", loud: false) == "z0");
}

test("named arguments are full expressions") {
    let k = 2;
    assert!(digits(c: k + 1, a: k - 1, b: k) == 123);
    assert!(digits(b: digits(0, 0, 5), a: 0, c: 0) == 50);
}
