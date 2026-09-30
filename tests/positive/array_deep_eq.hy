use string::to_bytes;

test("array deep equality") {
    let a = to_bytes("hi");
    let b = to_bytes("hi");
    let c = to_bytes("ho");
    assert(a == b)?;
    assert(!(a == c))?;
    assert(a != c)?;
}

test("int vec deep equality") {
    let a: Vec<int> = Vec::new();
    a.push(1);
    a.push(2);
    let b: Vec<int> = Vec::new();
    b.push(1);
    b.push(2);
    let c: Vec<int> = Vec::new();
    c.push(1);
    c.push(3);
    assert(a == b)?;
    assert(!(a == c))?;
}
