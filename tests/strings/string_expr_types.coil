// `+` on two strings and `string::format` both give a `string`, and a match
// whose arms are strings feeds `%s`.
use tests::assert;

test("string concatenation is a string") {
    assert!((typeof ("hello" + "world")) == "string");
    assert!("hello" + "world" == "helloworld");
}

test("format returns a string") {
    let s = string::format("%i-%s", 42, "x");
    assert!((typeof s) == "string");
    assert!(s == "42-x");
}

test("a match of strings formats with %s") {
    let s = match Option::Some(1) {
        Option::None => "none",
        Option::Some(_) => "some",
    };
    assert!(string::format("%s", s) == "some");
}
