// Ground tuples and fixed arrays have structural `Eq` and `Hash` instances:
// `eq` / `ne` compare as `==` / `!=` already do, and `hash` combines the
// items' hashes as a derived `Hash` combines fields. They meet `T: Eq` /
// `T: Hash` bounds through a monomorphic clone and a shared generic body
// (a `PolyFn` value), so a tuple can key a `HashMap` (#1479).
use tests::assert;
use collections::map::HashMap;

#[derive(Eq, Hash)]
class Point {
    pub x: int,
    pub y: int,
}

fn same<T: Eq>(T a, T b) -> bool {
    return a == b;
}

fn differ<T: Eq>(T a, T b) -> bool {
    return a != b;
}

fn hashed<T: Hash>(T x) -> int {
    return x.hash();
}

// A derived `Hash`'s combination of two hashes.
fn combine(int a, int b) -> int {
    return ((a & 288230376151711743) * 31) ^ b;
}

test("eq and ne of tuples and arrays") {
    assert!((1, 2).eq((1, 2)));
    assert!((1, 2).ne((1, 3)));
    assert!(!(1, "a").eq((1, "b")));
    assert!([1, 2, 3].eq([1, 2, 3]));
    assert!([1, 2].ne([2, 1]));
    assert!(((), 1).eq(((), 1)));
}

test("a tuple or array meets an Eq bound") {
    assert!(same((1, 2), (1, 2)));
    assert!(!same((1, 2), (2, 1)));
    assert!(differ(("a", 1.5), ("a", 2.5)));
    assert!(same([1, 2], [1, 2]));
    assert!(!same(["x"], ["y"]));
    assert!(same(((1, 2), [3]), ((1, 2), [3])));
    assert!(
        same((new Point(1, 2), 3), (new Point(1, 2), 3)) ==
        ((new Point(1, 2), 3) == (new Point(1, 2), 3)),
    );
}

test("an Eq bound through a shared body") {
    let s = same;
    let d = differ;
    assert!(s((1, 2), (1, 2)));
    assert!(!s((1, 2), (1, 9)));
    assert!(d([1], [2]));
    assert!(!d([true, false], [true, false]));
}

test("hash of tuples and arrays") {
    assert!((1, 2).hash() == combine(1.hash(), 2.hash()), (1, 2).hash());
    assert!((1, 2).hash() == (1, 2).hash());
    assert!(("a", true, 3).hash() == combine(combine("a".hash(), true.hash()), 3.hash()));
    assert!([1, 2, 3].hash() == combine(combine(1.hash(), 2.hash()), 3.hash()));
    assert!(((), 5).hash() == combine(0, 5.hash()));
    let p = new Point(1, 2);
    assert!((p, [1]).hash() == combine(p.hash(), [1].hash()));
    assert!(((1, 2), 3).hash() == combine((1, 2).hash(), 3.hash()));
}

test("a tuple or array meets a Hash bound") {
    assert!(hashed((1, 2)) == (1, 2).hash());
    assert!(hashed([4, 5]) == [4, 5].hash());
    let h = hashed;
    assert!(h((1, 2)) == (1, 2).hash());
    assert!(h(["a", "b"]) == ["a", "b"].hash());
}

test("a tuple keys a HashMap") {
    let m: HashMap<(int, int), string> = HashMap::new();
    m.insert((1, 2), "a");
    m.insert((2, 1), "b");
    m.insert((1, 2), "c");
    assert!(m.size() == 2);
    assert!(m.get((1, 2), "") == "c");
    assert!(m.get((2, 1), "") == "b");
    assert!(m.get((3, 3), "none") == "none");
}
