// Same-arity overloads on `int`, `float`, `string` and `bool` pick the
// candidate whose parameter type matches each argument; overloads may also
// differ in their return types.
use tests::assert;
use string::format;

fn kind(int x) -> string {
    return format("i%i", x);
}

fn kind(float x) -> string {
    return "f";
}

fn kind(string x) -> string {
    return "s" + x;
}

fn kind(bool x) -> string {
    if x {
        return "T";
    }
    return "F";
}

fn twice(int x) -> int {
    return x * 2;
}

fn twice(string x) -> string {
    return x + x;
}

fn pair(int a, string b) -> string {
    return "is";
}

fn pair(string a, int b) -> string {
    return "si";
}

test("one candidate per primitive type") {
    assert!(kind(3) == "i3");
    assert!(kind(-1) == "i-1");
    assert!(kind(1.5) == "f");
    assert!(kind("x") == "sx");
    assert!(kind(true) == "T");
    assert!(kind(false) == "F");
}

test("the argument's inferred type picks the overload") {
    let n = 2 + 2;
    let s = "a" + "b";
    assert!(kind(n) == "i4");
    assert!(kind(s) == "sab");
    assert!(kind(n > 3) == "T");
    assert!(kind("abc".len()) == "i3");
}

test("overloads with different return types") {
    assert!(twice(21) == 42);
    assert!(twice("ab") == "abab");
    assert!(kind(twice(5)) == "i10");
    assert!(kind(twice("z")) == "szz");
}

test("parameter order distinguishes overloads") {
    assert!(pair(1, "a") == "is");
    assert!(pair("a", 1) == "si");
}
