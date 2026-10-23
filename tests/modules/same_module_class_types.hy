// A non-entry module's own classes keep its namespace in a later fn's
// signature, a recursive method's return (#1980) and an enum payload (#1983),
// generic classes too.

use tests::assert;
use local_class_sigs::{make, make_guard, spec_kids, cell_value};
use local_payload::{make_ev, ev_value, make_gv, gv_value};

test("later-declared fn with a local class parameter") {
    assert!(make());
}

test("later-declared fn returning Result of a local class") {
    assert!(make_guard() == 7);
}

test("recursive method returning a local class") {
    assert!(spec_kids() == 2);
}

test("enum payload of a local class") {
    assert!(ev_value(make_ev(3)) == 6);
}

test("later-declared fn with a generic local class parameter") {
    assert!(cell_value() == 5);
}

test("enum payload of a generic local class") {
    assert!(gv_value(make_gv(3)) == 10);
}
