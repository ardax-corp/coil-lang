// A `Vec` local bound to an array literal keeps its items in frame slots
// until it escapes; direct MIR reads and writes those slots.
use tests::assert;

fn total(int a, int b) -> int {
    let v: Vec<int> = [a, b, a + b];
    v[1] = v[1] * 10;
    let s = 0;
    for x in v {
        s = s + x;
    }
    return s + v[2];
}

fn grown(int a) -> int {
    let v: Vec<int> = [a, a];
    v[0] = 7;
    v.push(a * 3);
    return v[0] + v[1] + v[2] + v.len();
}

fn floats(float x) -> float {
    let v: Vec<float> = [x, x * 2.0];
    return v[0] + v[1];
}

test("Vec literal locals in frame slots") {
    assert!(total(1, 2) == 27);
    assert!(grown(2) == 18);
    assert!(floats(1.5) == 4.5);
}
