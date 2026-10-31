// Range literals are `Range<T>` / `RangeInclusive<T>` of their bound type,
// `.to_vec()` gives a `Vec` of that type, and `for … in` binds the element
// type of a range or array.
use tests::assert;

test("range literal types") {
    assert!((typeof (0..10)) == "prelude::Range<int>");
    assert!((typeof (0..=10)) == "prelude::RangeInclusive<int>");
    assert!((typeof (1.0..2.0)) == "prelude::Range<float>");
}

test("to_vec keeps the element type") {
    let v = (0..5).to_vec();
    assert!((typeof v) == "prelude::Vec<int>");
    let r = 0..=3;
    let w = r.to_vec();
    assert!((typeof w) == "prelude::Vec<int>");
    assert!(w.len() == 4);
    let lo: byte = 1;
    let hi: byte = 4;
    let b = (lo..hi).to_vec();
    assert!((typeof b) == "prelude::Vec<byte>");
    let f = (1.0..=3.0).to_vec();
    assert!((typeof f) == "prelude::Vec<float>");
    assert!(f.len() == 3);
}

test("for over a range or an array binds the element type") {
    let sum = 0;
    for x in 0..5 {
        let y = x;
        assert!((typeof y) == "int");
        sum += y;
    }
    assert!(sum == 10);
    for x in [1, 2, 3] {
        let y = x;
        assert!((typeof y) == "int");
    }
}
