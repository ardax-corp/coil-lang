// #957: a class or enum declared under the same name in two modules must not
// clash on the default `Show` / `String` instances (or derived ones).
use tests::assert;
use string::format;
use same_name_types::{make_reader, reader_n, fast, show_reader};

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
    let m2 = fast();
    assert!(format("%v", m2) == "Fast");
}

test("each class shows through its own instance") {
    assert!(show_reader(3) == "Reader { n: 3 }");
    let r = new Reader(1, "x");
    let s = format("%v", r);
    assert!(s.len() > 0);
}
