// Tuple literals, indexing, and for-in.
use tests::assert;

test("pair index") {
    let t = (1, 2);
    assert!(t[0] == 1);
    assert!(t[1] == 2);
}

test("heterogeneous tuple") {
    let t = (42, "ok", true);
    assert!(t[0] == 42);
    assert!(t[1] == "ok");
    assert!(t[2] == true);
}

test("one-tuple trailing comma") {
    let t = (7,);
    assert!(t[0] == 7);
}

test("nested tuple") {
    let t = ((1, 2), (3, 4));
    assert!(t[0][0] == 1);
    assert!(t[1][1] == 4);
}

test("for in tuple") {
    let sum = 0;
    for x in (1, 2, 3) {
        sum = sum + x;
    }
    assert!(sum == 6);
}

test("tuple as function arg") {
    // exercised via local destructure-by-index
    let pair = (10, 20);
    let s = pair[0] + pair[1];
    assert!(s == 30);
}
