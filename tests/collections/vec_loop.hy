// Counted map and reduce loops run 8 lanes at a time in one op
// (`mir/vec_loop.rs`), wherever they sit in a function. Each is checked
// against the same loop kept scalar (`black_box` in its body), at lengths
// around the 8 lanes and from several starts.
use tests::assert;
use thread::{join, spawn, ThreadError};

fn sum(Vec<int> xs, int from) -> int {
    if xs.len() == 0 {
        return -1;
    }
    let s = 0;
    let i = from;
    while i < xs.len() {
        s = s + xs[i];
        i = i + 1;
    }
    return s;
}

fn sum_scalar(Vec<int> xs, int from) -> int {
    if xs.len() == 0 {
        return -1;
    }
    let s = 0;
    let i = from;
    while i < xs.len() {
        s = s + black_box(xs[i]);
        i = i + 1;
    }
    return s;
}

fn weighted(Vec<int> xs, int k) -> int {
    let s = 7;
    let i = 0;
    while i < xs.len() - 1 {
        s = s + ((xs[i + 1] - xs[i]) * k + i);
        i = i + 1;
    }
    return s;
}

fn weighted_scalar(Vec<int> xs, int k) -> int {
    let s = 7;
    let i = 0;
    while i < xs.len() - 1 {
        s = s + black_box((xs[i + 1] - xs[i]) * k + i);
        i = i + 1;
    }
    return s;
}

fn dot(Vec<float> a, Vec<float> b) -> float {
    let s = 0.5;
    let i = 0;
    while i < a.len() {
        s = s + a[i] * b[i];
        i = i + 1;
    }
    return s;
}

fn dot_scalar(Vec<float> a, Vec<float> b) -> float {
    let s = 0.5;
    let i = 0;
    while i < a.len() {
        s = s + black_box(a[i] * b[i]);
        i = i + 1;
    }
    return s;
}

fn ints(int n) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        v.push((i * 7919) % 1001 - 500);
        i = i + 1;
    }
    return v;
}

fn floats(int n) -> Vec<float> {
    let v: Vec<float> = Vec::new();
    let i = 0;
    while i < n {
        v.push(((i * 31) % 97) as float * 0.1 + 0.3);
        i = i + 1;
    }
    return v;
}

test("reductions at every length and start") {
    let lens = [0, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 100];
    for n in lens {
        let xs = ints(n);
        let from = 0;
        while from < 12 {
            assert!(sum(xs, from) == sum_scalar(xs, from));
            from = from + 1;
        }
        if n > 0 {
            assert!(weighted(xs, 3) == weighted_scalar(xs, 3));
        }
        let fs = floats(n);
        assert!(dot(fs, fs) == dot_scalar(fs, fs));
    }
}

fn scale(Vec<float> out, Vec<float> a, float k) -> int {
    if out.len() < a.len() {
        return -1;
    }
    let i = 0;
    while i < a.len() {
        out[i] = a[i] * k + 1.0;
        i = i + 1;
    }
    return i;
}

fn index_into(Vec<int> out, int base) {
    let i = 0;
    while i < out.len() {
        out[i] = i * 2 + base;
        i = i + 1;
    }
}

// `a[i] = a[i + 1]` reads ahead of what it writes: a shift left.
fn shift_left(Vec<int> a) {
    let i = 0;
    while i < a.len() - 1 {
        a[i] = a[i + 1];
        i = i + 1;
    }
}

// Reads behind what it writes: each element sees the one just written.
fn running(Vec<int> a) {
    let i = 1;
    while i < a.len() {
        a[i] = a[i - 1] + 1;
        i = i + 1;
    }
}

test("map loops") {
    let a = floats(37);
    let out = floats(37);
    assert!(scale(out, a, 2.0) == 37);
    let i = 0;
    while i < 37 {
        assert!(out[i] == a[i] * 2.0 + 1.0);
        i = i + 1;
    }
    assert!(scale(floats(3), a, 2.0) == -1);
    let ix = ints(29);
    index_into(ix, 5);
    assert!(ix[0] == 5 && ix[17] == 39 && ix[28] == 61);
    let s = ints(30);
    let want = ints(30);
    shift_left(s);
    let j = 0;
    while j < 29 {
        assert!(s[j] == want[j + 1]);
        j = j + 1;
    }
    let r = ints(30);
    running(r);
    assert!(r[29] == r[0] + 29);
}

// An index reduce: `s = s + i`.
fn tri(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}

test("index reduce") {
    assert!(tri(0) == 0);
    assert!(tri(9) == 36);
    assert!(tri(100) == 4950);
}

fn overflow() -> int {
    let xs: Vec<int> = Vec::new();
    let i = 0;
    while i < 40 {
        xs.push(9223372036854775807 / 30);
        i = i + 1;
    }
    return sum(xs, 0);
}

fn divide_by_zero() -> int {
    let xs = ints(40);
    let zs: Vec<int> = Vec::new();
    let i = 0;
    while i < 40 {
        zs.push(if i == 21 {
            0
        } else {
            3
        });
        i = i + 1;
    }
    let s = 0;
    let j = 0;
    while j < 40 {
        s = s + xs[j] / zs[j];
        j = j + 1;
    }
    return s;
}

fn read_past_end() -> int {
    let a = ints(20);
    let s = 0;
    let i = 0;
    while i < 30 {
        s = s + a[i];
        i = i + 1;
    }
    return s;
}

fn panics(() -> int f) -> bool {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Err(ThreadError::JoinFailed) => true,
        default => false,
    };
}

test("overflow, a zero divisor and a read past the end still panic") {
    assert!(panics(overflow));
    assert!(panics(divide_by_zero));
    assert!(panics(read_past_end));
}
