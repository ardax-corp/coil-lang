// A module's own calls of its `... xs` function each get a copy for their
// pack; its classes, enums, traits and overloads still load once
// (coil-lang#1281).
use tests::assert;
use variadic_provider::{summary, twice};

test("a module calls its variadic function with two packs") {
    assert!(summary(5, "x") == 14);
    assert!(twice("ab") == "abab");
}
