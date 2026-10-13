use tests::assert;

test("bool four") {
    let b = [true, false, true, false];
    assert!(b[0] == true);
    assert!(b[1] == false);
    assert!(b[2] == true);
    assert!(b[3] == false);
}

test("float four") {
    let f = [1.0, 2.0, 3.0, 4.0];
    assert!(f[0] == 1.0);
    assert!(f[3] == 4.0);
    f[2] = 8.0;
    assert!(f[2] == 8.0);
}
