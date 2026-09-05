test("compound index assign keeps the written value") {
    let a: Vec<int> = Vec::from([1, 2, 3]);
    a[1] += 10;
    assert(a[1] == 12);
    let x = a[0];
    let y = a[0];
    assert(x + y == 2);
}

test("guarded index after i < len") {
    let a: Vec<int> = Vec::from([4, 5, 6]);
    let i = 2;
    let v = 0;
    if i < len(a) {
        v = a[i];
    }
    assert(v == 6);
}
