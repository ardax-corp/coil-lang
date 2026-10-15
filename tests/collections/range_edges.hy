// Range edge cases: empty, singleton and decreasing ranges in `for` and
// `to_vec`, negative bounds, bounds evaluated once, reusable range values,
// byte ranges up to 255 and float ranges.
use tests::assert;
use string::format;

test("empty and singleton ranges") {
    let n = 0;
    for x in 3..3 {
        n = n + 1;
    }
    assert!(n == 0);
    for x in 3..=3 {
        n = n + x;
    }
    assert!(n == 3);
    for x in 5..2 {
        n = n + 100;
    }
    for x in 5..=4 {
        n = n + 100;
    }
    assert!(n == 3);
    assert!((3..3).to_vec().len() == 0);
    assert!((5..=4).to_vec().len() == 0);
    assert!((3..=3).to_vec() == Vec::from([3]));
}

test("negative bounds") {
    let v = (-3..1).to_vec();
    assert!(v == Vec::from([-3, -2, -1, 0]));
    let w = (-2..=-1).to_vec();
    assert!(w.len() == 2 && w[0] == -2);
}

test("bounds evaluated once") {
    let k = 0;
    let n = 0;
    for x in 0..(k + 3) {
        k = k + 10;
        n = n + 1;
    }
    assert!(n == 3);
}

test("range as value") {
    let r = 1..4;
    let s = 0;
    for x in r {
        s = s + x;
    }
    for x in r {
        s = s + x;
    }
    assert!(s == 12);
}

test("byte and float") {
    let b: byte = 250;
    let n = 0;
    let e: byte = 255;
    for x in b..=e {
        n = n + 1;
    }
    assert!(n == 6, format("%i", n));
    let f = (0.5..3.0).to_vec();
    assert!(f.len() == 3 && f[2] == 2.5);
}

test("break continue in range") {
    let s = 0;
    for x in 0..100 {
        if x % 2 == 0 {
            continue;
        }
        if x > 9 {
            break;
        }
        s = s + x;
    }
    assert!(s == 25);
}

test("byte to_vec at the top") {
    let lo: byte = 253;
    let hi: byte = 255;
    let v = (lo..=hi).to_vec();
    assert!(v.len() == 3, format("%i", v.len()));
}
