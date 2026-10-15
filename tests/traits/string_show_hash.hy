// `s.show()` and `s.hash()` on a string call the builtin thunk with the
// string's own word; a dictionary (a shared generic body, `%v`) reaches
// the same text and hash through the thunk's `$dict` adapter.
use tests::assert;
use string::format;

#[derive(Show, Hash, Eq)]
enum Mode {
    Read = "r",
    Write = "w",
}

#[derive(Show, Hash, Eq)]
class Tag {
    pub name: string,
    pub n: int,
}

fn shown<T: Show>(T x) -> string {
    return x.show();
}

fn hashed<T: Hash>(T x) -> int {
    return x.hash();
}

test("string show direct and through a dictionary") {
    let s = "abc";
    assert!(s.show() == "abc");
    assert!(shown(s) == "abc");
    assert!(shown(Mode::Write) == "w");
    assert!(format("%v", Mode::Read) == "r");
    assert!(new Tag("x", 2).show() == shown(new Tag("x", 2)));
}

test("string hash direct and through a dictionary") {
    let a = "hello";
    let b = "hel" + "lo";
    assert!(a.hash() == b.hash());
    assert!(a.hash() == hashed(b));
    assert!(a.hash() != "world".hash());
    assert!(Mode::Read.hash() == hashed(Mode::Read));
    assert!(Mode::Read.hash() != Mode::Write.hash());
    assert!(new Tag("x", 2).hash() == hashed(new Tag("x", 2)));
}
