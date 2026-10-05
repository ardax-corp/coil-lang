// `Vec::from(array)` lowered from HIR: scalar and heap elements, a literal
// and a local array, and growth after the copy.

class Node {
    pub v: int,
}

fn sum(Vec<int> v) -> int {
    let total = 0;
    for x in v {
        total = total + x;
    }
    return total;
}

test("from a literal") {
    let v = Vec::from([1, 2, 3]);
    v.push(4);
    assert(v.len() == 4)?;
    assert(sum(v) == 10)?;
}

test("from a local array") {
    let a = [5, 6];
    let v: Vec<int> = Vec::from(a);
    v.push(7);
    assert(sum(v) == 18)?;
    assert(a[0] == 5)?;
}

test("heap elements") {
    let v = Vec::from([new Node(1), new Node(2)]);
    v.push(new Node(3));
    let total = 0;
    for n in v {
        total = total + n.v;
    }
    assert(total == 6)?;
}
