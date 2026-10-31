// Calls of an imported function or method with fewer arguments than it
// declares are partial applications, as local ones are (#1231, #1361).
use tests::assert;
use partial_provider::{add3, Acc};

test("an imported function under-applies") {
    let f = add3(1);
    let g = add3(1, 2);
    assert!(f(2, 3) == 123);
    assert!(g(4) == 124);
    assert!(add3(7)(8, 9) == 789);
}

test("a qualified call under-applies") {
    let q = partial_provider::add3(4);
    assert!(q(5, 6) == 456);
}

test("an imported class's method under-applies") {
    let acc = new Acc(500);
    let m = acc.m(3);
    assert!(m(4) == 534);
}
