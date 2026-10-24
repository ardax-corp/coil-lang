// A typed `T... xs` function called from another module packs the trailing
// arguments, as a call in its own module does (#1990).
use tests::assert;
use typed_rest_provider::{total, joined, local_total};

test("an imported typed rest function packs its arguments") {
    assert!(total(1, 2, 3) == 6);
    assert!(total() == 0);
    assert!(total(5) == 5);
    assert!(local_total() == 6);
    assert!(typed_rest_provider::total(4, 4) == 8);
}

test("fixed arguments before an imported typed rest") {
    assert!(joined("-", "a", "b", "c") == "a-b-c");
    assert!(joined(",") == "");
}
