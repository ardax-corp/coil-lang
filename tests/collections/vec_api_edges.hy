// `Vec` API at its edges: `pop` / `remove` on empty or out-of-range give
// `None`, `insert` at both ends and into an empty Vec, capacity, `clear`,
// index writes, equality, and `Vec::from` copying its source array.
use tests::assert;

test("pop on empty gives None") {
    let v: Vec<int> = Vec::new();
    assert!((v.pop() ?? -1) == -1);
    v.push(4);
    assert!((v.pop() ?? -1) == 4);
    assert!((v.pop() ?? -1) == -1);
    assert!(v.len() == 0);
}

test("remove returns the element or None out of range") {
    let v = Vec::from([1, 2, 3, 4]);
    assert!((v.remove(0) ?? -1) == 1);
    assert!((v.remove(2) ?? -1) == 4);
    assert!((v.remove(5) ?? -1) == -1);
    assert!((v.remove(-1) ?? -1) == -1);
    assert!(v.len() == 2);
    assert!(v[0] == 2 && v[1] == 3);
}

test("insert at both ends and into an empty Vec") {
    let v = Vec::from([2, 3]);
    v.insert(0, 1);
    v.insert(3, 4);
    v.insert(2, 9);
    assert!(v == Vec::from([1, 2, 9, 3, 4]));
    let e: Vec<int> = Vec::new();
    e.insert(0, 7);
    assert!(e.len() == 1 && e[0] == 7);
}

test("with_capacity, reserve and clear") {
    let v: Vec<int> = Vec::with_capacity(10);
    assert!(v.len() == 0);
    assert!(v.capacity() >= 10);
    v.reserve(50);
    assert!(v.capacity() >= 50);
    v.push(1);
    v.clear();
    assert!(v.len() == 0);
    v.push(2);
    assert!(v[0] == 2);
}

test("equality is element-wise") {
    let v = Vec::from([1, 2]);
    assert!(Vec::from([1, 2]) == Vec::from([1, 2]));
    assert!(Vec::from([1, 2]) != Vec::from([1, 2, 3]));
}

test("from copies the array, assignment shares the Vec") {
    let a = [1, 2, 3];
    let v = Vec::from(a);
    v[0] = 9;
    assert!(a[0] == 1);
    let w = v;
    w.push(4);
    assert!(v.len() == 4);
}

test("index writes, compound and increment") {
    let v = Vec::from([1, 2, 3]);
    v[2] = 30;
    v[0] += 5;
    v[1]++;
    assert!(v[0] == 6 && v[1] == 3 && v[2] == 30);
}

test("heap elements through pop, remove and insert") {
    let v: Vec<string> = Vec::new();
    assert!((v.pop() ?? "none") == "none");
    v.push("b");
    v.insert(0, "a");
    v.insert(2, "c");
    assert!((v.remove(3) ?? "none") == "none");
    assert!((v.remove(1) ?? "none") == "b");
    assert!((v.pop() ?? "none") == "c");
    assert!(v.len() == 1 && v[0] == "a");
}
