// A non-entry module's own classes keep its namespace in a later fn's
// signature, a recursive method's return (#1980) and an enum payload (#1983).

use tests::assert;
use local_class_sigs::{make, make_guard, spec_kids};
use local_payload::{make_ev, ev_value};

test("later-declared fn with a local class parameter") {
    assert!(make());
}

test("later-declared fn returning Result of a local class") {
    assert_eq!(make_guard(), 7);
}

test("recursive method returning a local class") {
    assert_eq!(spec_kids(), 2);
}

test("enum payload of a local class") {
    assert_eq!(ev_value(make_ev(3)), 6);
}
