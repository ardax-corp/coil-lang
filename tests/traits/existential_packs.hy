// Concrete values packed as a bare-class existential (`Show x`) where a
// body lowers on direct MIR: each pack boxes the word and pairs it with
// the instance's dictionary.

use tests::assert;

fn shown(Show x) -> string {
    return show(x);
}

fn joined(Show a, int n, Show b) -> string {
    return show(a) + show(n) + show(b);
}

test("packs of each word kind") {
    assert!(shown(3) == "3");
    assert!(shown(true) == "true");
    assert!(shown(2.5) == "2.5");
    assert!(shown("s") == "s");
}

test("packs around a plain argument") {
    assert!(joined(1, 2, "x") == "12x");
}

test("packs in a loop") {
    let out = "";
    for i in 0..3 {
        out = out + shown(i * 2);
    }
    assert!(out == "024");
}
