// An explicit `use m::ord` shadows the prelude `ord`; names not imported
// still resolve to the prelude (#1488).
use tests::assert;
use prelude_clash::{ord, diff, char as pchar};

test("imported fn shadows prelude ord") {
    assert!(ord("a") == 100);
}

test("unimported prelude name stays the builtin") {
    assert!(char(97)? == "a");
    assert!(pchar(97) == 200);
}

test("imported fn shadows prelude diff") {
    assert!(diff(7, 2) == 5);
}

test("imported fn shadows prelude ord inside a lambda") {
    let f = fn (string s) => ord(s);
    assert!(f("x") == 100);
}
