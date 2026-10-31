// Without `use io::{IoError}`, `IoError::Other` does not reserve the name
// `Other`: a user enum may have an `Other` case, written bare or qualified.
use tests::assert;

enum Foo {
    Bar,
    Other,
}

test("a user Other case") {
    let x = Foo::Other;
    let y = Other;
    assert!((typeof x) == "Foo");
    assert!((typeof y) == "Foo");
}
