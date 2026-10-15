// `break` and `continue` work in a `for … in` over every iterable kind:
// a Vec, a tuple, a dict, a coroutine and an inclusive range, and a byte
// range that ends at 255 stops instead of wrapping.
use tests::assert;

gen fn naturals() {
    let i = 1;
    while true {
        yield i;
        i = i + 1;
    }
}

test("Vec and tuple") {
    let v: Vec<int> = [3, 8, 1, 9, 4];
    let s = 0;
    for x in v {
        if x == 1 {
            continue;
        }
        if x == 9 {
            break;
        }
        s = s + x;
    }
    assert!(s == 11);
    let t = 0;
    for x in (1, 2, 3, 4) {
        if x % 2 == 0 {
            continue;
        }
        t = t + x;
    }
    assert!(t == 4);
}

test("dict entries") {
    let d = { a: 1, b: 20, c: 300 };
    let s = 0;
    for (k, v) in d {
        if k == "b" {
            continue;
        }
        s = s + v;
    }
    assert!(s == 301);
}

test("break out of an endless coroutine") {
    let s = 0;
    for n in naturals() {
        if n > 4 {
            break;
        }
        s = s + n;
    }
    assert!(s == 10);
}

test("inclusive range") {
    let s = 0;
    for i in 1..=10 {
        if i % 3 != 0 {
            continue;
        }
        s = s + i;
    }
    assert!(s == 18);
}

test("a byte range up to 255 terminates") {
    let lo: byte = 0;
    let hi: byte = 255;
    let n = 0;
    for b in lo..=hi {
        n = n + 1;
    }
    assert!(n == 256);
}
