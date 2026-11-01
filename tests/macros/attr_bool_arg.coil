// #1206: a `bool` attribute-macro argument reaches the macro as passed
// (`flags(FnDecl f, string label, int times, bool loud)` adds `!` when loud).
use tests::assert;
use macro_probe::flags;

#[flags(label = "hey", times = 2, loud = true)]
fn loud() -> int {
    return 1;
}

#[flags(label = "hm", times = 1, loud = false)]
fn quiet() -> int {
    return 2;
}

test("a true bool argument is true") {
    assert!(loud_flags() == "hey,hey!");
}

test("a false bool argument is false") {
    assert!(quiet_flags() == "hm");
}
