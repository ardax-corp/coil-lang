// The prelude `Eq` trait gives `ne` a default body, `!eq(a, b)`, like a
// user trait's default method: an `impl Eq` writes `eq` only and gets
// `!=` for free, directly, in a `Vec` and under a `T: Eq` bound. An impl
// that writes `ne` keeps its own (#2089).
use tests::assert;

class Mat {
    pub rows: Vec<int>,
}

impl Eq for Mat {
    fn eq(Mat a, Mat b) -> bool {
        return a.rows == b.rows;
    }
}

// `ne` written by hand replaces the default: it compares only `tag`, so
// values that `eq` tells apart by `note` are still "not different".
class Tagged {
    pub tag: int,
    pub note: string,
}

impl Eq for Tagged {
    fn eq(Tagged a, Tagged b) -> bool {
        return a.tag == b.tag && a.note == b.note;
    }

    fn ne(Tagged a, Tagged b) -> bool {
        return a.tag != b.tag;
    }
}

fn same<T: Eq>(T a, T b) -> bool {
    return a == b;
}

fn differ<T: Eq>(T a, T b) -> bool {
    return a != b;
}

fn mat(Vec<int> rows) -> Mat {
    return new Mat(rows);
}

test("an impl with only eq gets != from the default ne") {
    assert!(mat(Vec::from([1, 2])) == mat(Vec::from([1, 2])));
    assert!(!(mat(Vec::from([1, 2])) != mat(Vec::from([1, 2]))));
    assert!(mat(Vec::from([1, 2])) != mat(Vec::from([2, 1])));
    assert!(!(mat(Vec::from([1, 2])) == mat(Vec::from([2, 1]))));
}

test("the default ne is reached as a method and as a function") {
    let a = mat(Vec::from([3]));
    let b = mat(Vec::from([4]));
    assert!(a.ne(b));
    assert!(ne(a, b));
    assert!(!a.ne(a));
    assert!(Eq::ne(a, b));
}

test("a Vec of the class compares its items with the default ne") {
    let xs = Vec::from([mat(Vec::from([1])), mat(Vec::from([2]))]);
    let ys = Vec::from([mat(Vec::from([1])), mat(Vec::from([2]))]);
    let zs = Vec::from([mat(Vec::from([1])), mat(Vec::from([3]))]);
    assert!(xs == ys);
    assert!(!(xs != ys));
    assert!(xs != zs);
    assert!(!(xs == zs));
}

test("the default ne is reached through a T: Eq bound") {
    assert!(same(mat(Vec::from([7])), mat(Vec::from([7]))));
    assert!(!differ(mat(Vec::from([7])), mat(Vec::from([7]))));
    assert!(differ(mat(Vec::from([7])), mat(Vec::from([8]))));
    assert!(!same(mat(Vec::from([7])), mat(Vec::from([8]))));
}

test("an impl that writes ne overrides the default") {
    let a = new Tagged(1, "x");
    let b = new Tagged(1, "y");
    assert!(!(a == b));
    assert!(!(a != b));
    assert!(!differ(a, b));
    assert!(new Tagged(1, "x") != new Tagged(2, "x"));
    assert!(differ(new Tagged(1, "x"), new Tagged(2, "x")));
}
