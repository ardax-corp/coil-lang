// `for x in e` iterates every builtin iterable: fixed arrays, Vecs,
// homogeneous tuples, coroutines, and `int` / `byte` / `float` ranges (half-open, inclusive,
// stored in a variable); a decreasing range and an empty Vec run zero times.
use tests::assert;

gen fn evens(int n) {
    let i = 0;
    while i < n {
        yield i * 2;
        i = i + 1;
    }
}

test("fixed array and Vec") {
    let s = 0;
    for x in [1, 2, 3] {
        s = s + x;
    }
    assert!(s == 6);
    let v: Vec<string> = Vec::new();
    v.push("a");
    v.push("b");
    let out = "";
    for w in v {
        out = out + w;
    }
    assert!(out == "ab");
}

test("homogeneous tuple") {
    let s = 0;
    for x in (10, 20, 30) {
        s = s + x;
    }
    assert!(s == 60);
}

test("coroutine") {
    let s = 0;
    for x in evens(4) {
        s = s + x;
    }
    assert!(s == 0 + 2 + 4 + 6);
}

test("int ranges") {
    let a = 0;
    for i in 0..4 {
        a = a + i;
    }
    assert!(a == 6);
    let b = 0;
    for i in 0..=4 {
        b = b + i;
    }
    assert!(b == 10);
    let c = 0;
    for i in -3..0 {
        c = c + i;
    }
    assert!(c == -6);
}

test("byte and float ranges") {
    let lo: byte = 250;
    let hi: byte = 255;
    let n = 0;
    for b in lo..=hi {
        n = n + (b as int);
    }
    assert!(n == 250 + 251 + 252 + 253 + 254 + 255);
    let f = 0.0;
    for x in 0.5..3.0 {
        f = f + x;
    }
    assert!(f == 0.5 + 1.5 + 2.5);
}

test("a range stored in a variable") {
    let r = 1..=3;
    let p = 1;
    for x in r {
        p = p * x;
    }
    assert!(p == 6);
    let again = 0;
    for x in r {
        again = again + x;
    }
    assert!(again == 6);
}

test("empty iterations") {
    let runs = 0;
    for i in 5..0 {
        runs = runs + 1;
    }
    for i in 3..3 {
        runs = runs + 1;
    }
    for i in 4..=3 {
        runs = runs + 1;
    }
    let v: Vec<int> = Vec::new();
    for x in v {
        runs = runs + 1;
    }
    assert!(runs == 0);
}
