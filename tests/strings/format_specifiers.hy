// Every `format` specifier on its argument type: `%i` (also for `byte`),
// `%x` / `%b` / `%u`, `%f`, `%z`, `%s`, `%v`, `%%`, and literal-only strings.
use tests::assert;
use string::format;

fn twice(int n) -> int {
    return n * 2;
}

test("int specifiers") {
    assert!(format("%i", -42) == "-42");
    assert!(format("%i", 0) == "0");
    assert!(format("%i", 9223372036854775807) == "9223372036854775807");
    assert!(format("%x", 255) == "ff", format("%x", 255));
    assert!(format("%b", 5) == "101", format("%b", 5));
    assert!(format("%u", 7) == "7", format("%u", 7));
    let b: byte = 200;
    assert!(format("%i", b) == "200");
}

test("float, bool and string specifiers") {
    assert!(format("%f", 1.5) == "1.5", format("%f", 1.5));
    assert!(format("%f", 2.0) == "2.0", format("%f", 2.0));
    assert!(format("%f", -0.25) == "-0.25");
    assert!(format("%z|%z", true, false) == "true|false");
    assert!(format("%z", 1 < 2) == "true");
    assert!(format("[%s]", "") == "[]");
    assert!(format("%s%s", "a", "b") == "ab");
}

test("percent escapes and literal-only strings") {
    assert!(format("100%%") == "100%");
    assert!(format("%%%i%%", 5) == "%5%");
    assert!(format("") == "");
    assert!(format("plain") == "plain");
}

test("arguments fill specifiers in order") {
    let s = format("%s=%i (%z) %f", "n", twice(21), false, 0.5);
    assert!(s == "n=42 (false) 0.5", s);
    assert!(format("%v-%v", 1, "x") == "1-x");
}

test("results compose") {
    let inner = format("<%i>", 1);
    let outer = format("[%s|%s]", inner, inner);
    assert!(outer == "[<1>|<1>]");
    assert!(len(outer) == 9);
}
