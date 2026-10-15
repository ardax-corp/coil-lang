// Numeric Range / RangeInclusive collect into Vec via inherent to_vec.
use tests::assert;

test("half-open int") {
    let v = (0..5).to_vec();
    assert!(v.len() == 5);
    assert!(v[0] == 0);
    assert!(v[4] == 4);
}

test("inclusive first-class range") {
    let r = 0..=3;
    let v = r.to_vec();
    assert!(v.len() == 4);
    assert!(v[0] == 0);
    assert!(v[3] == 3);
}

test("empty and decreasing") {
    let empty = (0..0).to_vec();
    assert!(empty.len() == 0);
    let down = (10..0).to_vec();
    assert!(down.len() == 0);
    let down_inc = (3..=1).to_vec();
    assert!(down_inc.len() == 0);
}

test("byte range") {
    let lo: byte = 5;
    let hi: byte = 7;
    let v = (lo..=hi).to_vec();
    assert!(v.len() == 3);
    assert!(v[0] == (5 as byte));
    assert!(v[2] == (7 as byte));
}

test("float exclusive and inclusive") {
    let half = (1.0..4.0).to_vec();
    assert!(half.len() == 3);
    assert!(half[0] == 1.0);
    assert!(half[2] == 3.0);
    let closed = (1.0..=3.0).to_vec();
    assert!(closed.len() == 3);
    assert!(closed[2] == 3.0);
}

test("singleton inclusive and decreasing float") {
    let one = (7..=7).to_vec();
    assert!(one.len() == 1);
    assert!(one[0] == 7);
    let empty_f = (3.0..1.0).to_vec();
    assert!(empty_f.len() == 0);
    let empty_fi = (2.0..=0.0).to_vec();
    assert!(empty_fi.len() == 0);
}

test("to_vec matches for-in elements") {
    let v = (0..=3).to_vec();
    let i = 0;
    for x in 0..=3 {
        assert!(v[i] == x);
        i = i + 1;
    }
    assert!(i == v.len());

    let f = (1.0..4.0).to_vec();
    let j = 0;
    for x in 1.0..4.0 {
        assert!(f[j] == x);
        j = j + 1;
    }
    assert!(j == f.len());
}
