// A non-generic enum or class with no `Show` derive or impl shows as its
// type name, through `%v` and through `show`.
use tests::assert;
use string::format;

enum Plain {
    A,
    B(int),
}

class Bare {
    pub n: int,
}

test("enum without Show prints its type name") {
    assert!(format("%v", Plain::B(3)) == "Plain", format("%v", Plain::B(3)));
    assert!(format("%v", Plain::A) == "Plain");
    assert!(show(Plain::A) == "Plain");
}

test("class without Show prints its type name") {
    assert!(format("%v", new Bare(1)) == "Bare", format("%v", new Bare(1)));
    let b = new Bare(2);
    assert!(b.show() == "Bare");
    assert!(format("<%v|%v>", b, Plain::A) == "<Bare|Plain>");
}
