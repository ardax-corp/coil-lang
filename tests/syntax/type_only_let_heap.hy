// #1273: a type-only `let x: T;` of a non-scalar type compiles when every
// read comes after an assignment on every path.
use tests::assert;
use gc::collect;

class Point {
    pub x: int,
    pub y: int,
}

fn pick(bool first) -> string {
    let s: string;
    if first {
        s = "one";
    } else {
        s = "two";
    }
    return s;
}

fn early(int n) -> string {
    let s: string;
    if n < 0 {
        return "negative";
    }
    s = "positive";
    return s;
}

fn by_match(int n) -> Point {
    let p: Point;
    match n {
        0 => {
            p = new Point(0, 0);
        },
        default => {
            p = new Point(n, n * 2);
        },
    }
    return p;
}

test("assigned before the read") {
    let s: string;
    s = "hi";
    assert!(s == "hi");
}

test("assigned on both branches") {
    assert!(pick(true) == "one");
    assert!(pick(false) == "two");
}

test("a path that returns early need not assign") {
    assert!(early(-1) == "negative");
    assert!(early(1) == "positive");
}

test("a class assigned in every match arm") {
    let p = by_match(3);
    assert!(p.x == 3);
    assert!(p.y == 6);
    assert!(by_match(0).x == 0);
}

test("a vector, reassigned in a loop") {
    let xs: Vec<int>;
    xs = [1];
    let i = 0;
    while i < 3 {
        let item: Vec<int>;
        item = [i];
        xs.push(item[0]);
        i += 1;
    }
    assert!(xs.len() == 4);
}

test("never read") {
    let unused: string;
    assert!(true);
}

test("a collection while the local is unassigned") {
    let p: Point;
    let junk = [new Point(1, 1), new Point(2, 2)];
    collect();
    p = new Point(junk.len(), 9);
    collect();
    assert!(p.x == 2);
    assert!(p.y == 9);
}
