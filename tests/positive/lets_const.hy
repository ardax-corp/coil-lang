// let / const bindings, reassignment, and slot isolation.
use tests::assert;

test("simple let and read") {
    let x = 5;
    assert!(x == 5);
}

test("reassignment") {
    let x = 5;
    x = 10;
    assert!(x == 10);
    x = x + 1;
    assert!(x == 11);
}

test("two bindings independent") {
    let x = 5;
    let y = 10;
    assert!(x + y == 15);
    y = 100;
    assert!(x == 5);
    assert!(y == 100);
}

test("chained let from prior binding") {
    let x = 5;
    let y = x + 1;
    let z = y * 2;
    assert!(z == 12);
}

test("const binding readable") {
    const answer = 42;
    assert!(answer == 42);
    const hi = "hi";
    assert!(hi == "hi");
}

test("block executes statements") {
    let x = 1;
    {
        x = 2;
    }
    assert!(x == 2);
}
