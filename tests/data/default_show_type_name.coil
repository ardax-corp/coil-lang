// A non-generic class with no `Show` derive or impl shows as its type name,
// through `%v` and through `show`; an enum shows its variant (#1749).
use tests::assert;
use string::format;

enum Plain {
    A,
    B(int),
}

class Bare {
    pub n: int,
}

test("enum without Show prints its variant name") {
    assert!(format("%v", Plain::B(3)) == "Plain::B", format("%v", Plain::B(3)));
    assert!(format("%v", Plain::A) == "Plain::A");
    assert!(show(Plain::A) == "Plain::A");
}

test("class without Show prints its type name") {
    assert!(format("%v", new Bare(1)) == "Bare", format("%v", new Bare(1)));
    let b = new Bare(2);
    assert!(b.show() == "Bare");
    assert!(format("<%v|%v>", b, Plain::A) == "<Bare|Plain::A>");
}
