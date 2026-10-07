// A trait method on a bare-class existential (`Show x`): a local pack
// loads per use at any depth; another pack stages through a temp.

use string::format;

fn described(Show x) -> string {
    return format("<%s>", show(x));
}

fn twice(Show x) -> string {
    return show(x) + show(x);
}

fn packed(int n) -> Show {
    return n;
}

test("existential argument") {
    assert(described(42) == "<42>")?;
    assert(described("hi") == "<hi>")?;
}

test("existential used twice") {
    assert(twice(7) == "77")?;
}

test("existential from a call") {
    assert(show(packed(5)) == "5")?;
}
