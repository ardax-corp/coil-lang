// #957: a class or enum declared under the same name in two modules must not
// clash on the default `Show` / `String` instances (or derived ones).
// Showing the module's enum is #1223.
use tests::assert;
use string::format;
use same_name_types::{make_reader, reader_n, fast, show_reader, is_fast};
use same_name_types_b::show_text_reader;

class Reader {
    pub n: int,
    pub label: string,
}

enum Mode {
    On,
    Off,
}

test("local class with an imported namesake builds and keeps its fields") {
    let r = new Reader(7, "local");
    assert!(r.n == 7);
    assert!(r.label == "local");
}

test("imported class with a local namesake keeps its own shape") {
    let other = make_reader(5);
    assert!(reader_n(other) == 5);
}

test("both enums keep their own cases") {
    let m = Mode::Off;
    let on = match m {
        Mode::On => true,
        Mode::Off => false,
    };
    assert!(!on);
    assert!(is_fast(fast()));
}

test("each class shows through its own instance") {
    assert!(show_reader(3) == "Reader { n: 3 }");
    assert!(format("%v", new Reader(1, "x")) == "Reader");
    assert!(format("%v", Mode::On) == "Mode");
    assert!(show_text_reader("t") == "Reader { text: t }");
}
