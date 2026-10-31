// A trailing `...v` of a `Vec<T>` fills a `T... xs` rest parameter whole
// (coil-lang#1151), so a rest pack can be forwarded.
use tests::assert;

fn count(string... xs) -> int {
    return xs.len();
}

fn total(int... xs) -> int {
    let sum = 0;
    for x in xs {
        sum += x;
    }
    return sum;
}

fn scaled(int k, int... xs) -> int {
    return k * total(...xs);
}

fn forward(int... xs) -> int {
    return total(...xs);
}

test("vec literal local spread into a rest") {
    let v: Vec<string> = ["x", "y", "z"];
    assert!(count(...v) == 3);
}

test("Vec::from spread into a rest") {
    let v = Vec::from([4, 5]);
    assert!(total(...v) == 9);
}

test("empty vec spread into a rest") {
    let v: Vec<int> = Vec::new();
    assert!(total(...v) == 0);
}

test("fixed argument before a vec spread") {
    let v = Vec::from([1, 2, 3]);
    assert!(scaled(10, ...v) == 60);
}

test("rest pack forwarded with a spread") {
    assert!(forward(1, 2, 3, 4) == 10);
    assert!(scaled(2, 5, 6) == 22);
}
