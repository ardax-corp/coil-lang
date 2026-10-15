// Exercised by `coil test`.
use tests::assert;

test("assert true") {
    assert!(true);
}

test("assert equality with message") {
    assert!(1 == 1, "ones");
}
