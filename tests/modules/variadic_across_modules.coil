// A `... xs` function called from another module: each argument list gets a
// copy made in the declaring module (coil-lang#1386).
use tests::assert;
use variadic_cross::{count, labelled, total, own};
use variadic_cross_user::pair_count;

test("an imported variadic function takes any argument list") {
    assert!(count() == 0);
    assert!(count(1, 2) == 2);
    assert!(count(true, 1.5, "x") == 3);
    assert!(variadic_cross::count(1) == 1);
}

test("the declaring module's own copies and another importer's are shared") {
    assert!(own() == 2);
    assert!(count(7, "b") == 2);
    assert!(pair_count() == 2);
}

test("fixed arguments, module items and nested template calls") {
    assert!(labelled("ab", 1, 2, 3) == 23);
    assert!(labelled("e") == 10);
    assert!(total(5, "x") == 3);
}
