// `show` / `hash` on int, float, bool and byte give the same result as a
// direct call and through a `Show` / `Hash` bound, and on a derived
// scalar enum.

use tests::assert;

#[derive(Show, Hash)]
enum Level {
    Low = 1,
    High = 9,
}

fn shown<T: Show>(T x) -> string {
    return x.show();
}

fn hashed<T: Hash>(T x) -> int {
    return x.hash();
}

test("ground show of each primitive") {
    let i = 42;
    let f = 2.5;
    let b = true;
    let y: byte = 7;
    assert!(i.show() == "42", i.show());
    assert!(f.show() == "2.5", f.show());
    assert!(b.show() == "true", b.show());
    assert!(y.show() == "7", y.show());
}

test("ground hash of each primitive") {
    let i = 42;
    let b = true;
    let y: byte = 7;
    assert!(i.hash() == 42);
    assert!(b.hash() == true.hash());
    assert!(y.hash() == 7);
    assert!(2.5.hash() == 2.5.hash());
    assert!(2.5.hash() != 3.5.hash());
}

test("dictionary show and hash agree with ground calls") {
    assert!(shown(42) == 42.show());
    assert!(shown(2.5) == 2.5.show());
    assert!(shown(false) == "false");
    assert!(hashed(42) == 42.hash());
    assert!(hashed(2.5) == 2.5.hash());
    assert!(hashed(true) == true.hash());
}

test("derived show and hash of a scalar enum") {
    assert!(Level::High.show() == "9", Level::High.show());
    assert!(shown(Level::Low) == "1", shown(Level::Low));
    assert!(Level::Low.hash() == hashed(Level::Low));
    assert!(Level::Low.hash() != Level::High.hash());
}
