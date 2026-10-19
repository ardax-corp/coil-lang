// Block comments nest to any depth, so commenting out code that already
// holds `/* … */` (or `//`) keeps the rest of the file intact; `*` and
// `/` alone inside a comment do not end it.
use tests::assert;

/*
fn removed() -> int {
    /* an inner note */
    return 1; // a line comment inside the block
}
*/

fn kept() -> int {
    /* level 1 /* level 2 /* level 3 */ back to 2 */ back to 1 */
    return 2;
}

/* a lone * and a lone / and ** and // do not close it */
fn also_kept() -> int {
    return 3;
}

test("nested comments close at the matching end") {
    assert!(kept() == 2);
    assert!(also_kept() == 3);
}

test("comments inside expressions") {
    let x = 1 + 2;
    /* plus */
    /* two */
    assert!(x == 3);
    let y = 4 * 2;
    /* /* nested */ */
    /* times */
    assert!(y == 8);
    let s = "/* not a comment */";
    assert!(s.len() == 19);
    let t = "// not a comment either";
    assert!(t.len() == 23);
}

test("division next to a comment") {
    let a = 8 / 2;
    /* by */
    assert!(a == 4);
    let b = 9 / 2;
    assert!(b == 4);
}
