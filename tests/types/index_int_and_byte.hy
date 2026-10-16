// An `int` or `byte` index still reads arrays, Vecs and strings (#1172).
use tests::assert;

test("int and byte indices") {
    let a = [10, 20, 30];
    let v = Vec::from([1, 2, 3]);
    let i = 2;
    let b: byte = 1;
    assert!(a[i] == 30);
    assert!(a[b] == 20);
    assert!(v[b] == 2);
    let c: byte = 99;
    assert!("abc"[i] == c);
}
