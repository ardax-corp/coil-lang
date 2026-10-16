// #1205: attribute macro arguments in order, by name, or positional ones
// followed by named ones (`flags(FnDecl f, string label, int times, bool loud)`).
use tests::assert;
use macro_probe::{flags};

#[flags("yo", 2, false)]
fn in_order() -> int {
    return 1;
}

#[flags("hi", times = 3, loud = false)]
fn mixed() -> int {
    return 2;
}

#[flags(label = "by", times = 1, loud = false)]
fn by_name() -> int {
    return 3;
}

test("every argument in order") {
    assert!(in_order_flags() == "yo,yo");
    assert!(in_order() == 1);
}

test("a positional argument followed by named ones") {
    assert!(mixed_flags() == "hi,hi,hi");
}

test("every argument by name") {
    assert!(by_name_flags() == "by");
}
