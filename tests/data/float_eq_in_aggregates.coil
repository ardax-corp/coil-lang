// `==` / `!=` on arrays, `Vec`s and tuples compare their `float` elements
// by IEEE 754, as scalar `float` `==` does: `NaN != NaN`, `0.0 == -0.0`
// (#1253).
use tests::assert;

fn nan() -> float {
    let z = 0.0;
    return z / z;
}

test("NaN inside an array or tuple is not equal to itself") {
    let n = nan();
    assert!(!([n] == [n]));
    assert!([n] != [n]);
    assert!(!((n, 1) == (n, 1)));
    assert!(!([[1.0, n]] == [[1.0, n]]));
}

test("zero and negative zero are equal inside aggregates") {
    assert!([0.0] == [-0.0]);
    let v: Vec<float> = Vec::new();
    v.push(-0.0);
    let w: Vec<float> = Vec::new();
    w.push(0.0);
    assert!(v == w);
    assert!(!(v != w));
    assert!((1.0, "a", [0.0]) == (1.0, "a", [-0.0]));
}

test("aggregates without floats compare as before") {
    assert!([1, 2] == [1, 2]);
    assert!((1, "a") != (1, "b"));
    assert!([1.5, 2.5] == [1.5, 2.5]);
}
