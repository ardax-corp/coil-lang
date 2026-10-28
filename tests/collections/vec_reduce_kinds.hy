// Min / max, count-if and bit reductions run in a vector loop
// (`vectorize::match_cond_reduce_on`, `VReduce` take and bit folds).
// Each is checked against the same loop kept scalar (`black_box`).
use tests::assert;

fn vmax(Vec<int> xs) -> int {
    let m = xs[0];
    let i = 1;
    while i < xs.len() {
        if xs[i] > m {
            m = xs[i];
        }
        i = i + 1;
    }
    return m;
}

fn vmax_scalar(Vec<int> xs) -> int {
    let m = xs[0];
    let i = 1;
    while i < xs.len() {
        if black_box(xs[i]) > m {
            m = xs[i];
        }
        i = i + 1;
    }
    return m;
}

// Written the other way round, and taken on the else side.
fn vmin(Vec<int> xs) -> int {
    let m = xs[0];
    let i = 0;
    while i < xs.len() {
        if m <= xs[i] {} else {
            m = xs[i];
        }
        i = i + 1;
    }
    return m;
}

fn vmin_scalar(Vec<int> xs) -> int {
    let m = xs[0];
    let i = 0;
    while i < xs.len() {
        if black_box(m) <= xs[i] {} else {
            m = xs[i];
        }
        i = i + 1;
    }
    return m;
}

// `>=` keeps the last of equal maxima: the index tells them apart.
fn last_max_at(Vec<int> xs) -> int {
    let m = xs[0];
    let i = 0;
    while i < xs.len() {
        if xs[i] * 1000 + i >= m {
            m = xs[i] * 1000 + i;
        }
        i = i + 1;
    }
    return m % 1000;
}

fn last_max_at_scalar(Vec<int> xs) -> int {
    let m = xs[0];
    let i = 0;
    while i < xs.len() {
        if black_box(xs[i] * 1000 + i) >= m {
            m = xs[i] * 1000 + i;
        }
        i = i + 1;
    }
    return m % 1000;
}

fn fmax(Vec<float> xs, float start) -> float {
    let m = start;
    let i = 0;
    while i < xs.len() {
        if xs[i] > m {
            m = xs[i];
        }
        i = i + 1;
    }
    return m;
}

fn fmax_scalar(Vec<float> xs, float start) -> float {
    let m = start;
    let i = 0;
    while i < xs.len() {
        if black_box(xs[i]) > m {
            m = xs[i];
        }
        i = i + 1;
    }
    return m;
}

fn count(Vec<int> xs, int k) -> int {
    let n = 0;
    let i = 0;
    while i < xs.len() {
        if xs[i] == k {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

fn count_scalar(Vec<int> xs, int k) -> int {
    let n = 0;
    let i = 0;
    while i < xs.len() {
        if black_box(xs[i]) == k {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

// Sum of the elements below a limit, the add on the else side.
fn sum_below(Vec<int> xs, int lim) -> int {
    let s = 0;
    let i = 0;
    while i < xs.len() {
        if xs[i] >= lim {} else {
            s = s + xs[i];
        }
        i = i + 1;
    }
    return s;
}

fn sum_below_scalar(Vec<int> xs, int lim) -> int {
    let s = 0;
    let i = 0;
    while i < xs.len() {
        if black_box(xs[i]) >= lim {} else {
            s = s + xs[i];
        }
        i = i + 1;
    }
    return s;
}

fn count_nan_or_big(Vec<float> xs) -> int {
    let n = 0;
    let i = 0;
    while i < xs.len() {
        if xs[i] != xs[i] {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

fn bits(Vec<int> xs) -> int {
    let a = -1;
    let o = 0;
    let x = 0;
    let i = 0;
    while i < xs.len() {
        a = a & xs[i];
        i = i + 1;
    }
    i = 0;
    while i < xs.len() {
        o = o | xs[i];
        i = i + 1;
    }
    i = 0;
    while i < xs.len() {
        x = x ^ xs[i];
        i = i + 1;
    }
    return a * 3 + o * 5 + x * 7;
}

fn bits_scalar(Vec<int> xs) -> int {
    let a = -1;
    let o = 0;
    let x = 0;
    let i = 0;
    while i < xs.len() {
        a = a & black_box(xs[i]);
        o = o | black_box(xs[i]);
        x = x ^ black_box(xs[i]);
        i = i + 1;
    }
    return a * 3 + o * 5 + x * 7;
}

fn mask(Vec<int> out, Vec<int> a, int m) {
    let i = 0;
    while i < a.len() {
        out[i] = (a[i] & m) ^ (a[i] | 1);
        i = i + 1;
    }
}

fn ints(int n, int seed) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        v.push((i * 7919 + seed) % 61 - 30);
        i = i + 1;
    }
    return v;
}

test("int min, max and the last of equal maxima") {
    let lens = [1, 7, 8, 9, 16, 17, 63, 64, 65, 200];
    for n in lens {
        let seed = 0;
        while seed < 5 {
            let xs = ints(n, seed);
            assert!(vmax(xs) == vmax_scalar(xs));
            assert!(vmin(xs) == vmin_scalar(xs));
            assert!(last_max_at(xs) == last_max_at_scalar(xs));
            seed = seed + 1;
        }
    }
}

test("float max with NaN and signed zeros") {
    let nan = 0.0 / 0.0;
    let xs: Vec<float> = Vec::new();
    let i = 0;
    while i < 40 {
        xs.push(
            if i % 9 == 4 {
                nan
            } else if i % 5 == 0 {
                -0.0
            } else {
                ((i * 13) % 17) as float - 8.0
            },
        );
        i = i + 1;
    }
    assert!(fmax(xs, -100.0) == fmax_scalar(xs, -100.0));
    assert!(fmax(xs, 0.0) == fmax_scalar(xs, 0.0));
    let from_nan = fmax(xs, nan);
    let want = fmax_scalar(xs, nan);
    assert!(from_nan == want || (from_nan != from_nan && want != want));
    assert!(count_nan_or_big(xs) == 4);
}

test("count and conditional sums") {
    let lens = [0, 5, 8, 31, 64, 129];
    for n in lens {
        let xs = ints(n, 3);
        let k = -30;
        while k <= 30 {
            assert!(count(xs, k) == count_scalar(xs, k));
            assert!(sum_below(xs, k) == sum_below_scalar(xs, k));
            k = k + 7;
        }
    }
}

test("bit folds and bit maps") {
    let lens = [0, 3, 8, 50];
    for n in lens {
        let xs = ints(n, 11);
        assert!(bits(xs) == bits_scalar(xs));
    }
    let a = ints(33, 2);
    let out = ints(33, 0);
    mask(out, a, 12);
    let i = 0;
    while i < 33 {
        assert!(out[i] == (a[i] & 12) ^ (a[i] | 1));
        i = i + 1;
    }
}
