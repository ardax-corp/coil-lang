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

// `a[base + j]` with a base the inner loop does not change: a flattened
// 2D array walked in a nest, and bases that put the lanes partly or
// wholly outside the array (those run the scalar loop and panic there).
fn rows(Vec<int> a, int w, int h, int base) -> int {
    let s = 0;
    let i = 0;
    while i < h {
        let row = base + i * w;
        let j = 0;
        while j < w {
            s = s + a[row + j];
            j = j + 1;
        }
        i = i + 1;
    }
    return s;
}

fn rows_scalar(Vec<int> a, int w, int h, int base) -> int {
    let s = 0;
    let i = 0;
    while i < h {
        let row = base + i * w;
        let j = 0;
        while j < w {
            s = s + black_box(a[row + j]);
            j = j + 1;
        }
        i = i + 1;
    }
    return s;
}

fn scale_rows(Vec<int> out, Vec<int> a, int w, int h) {
    let i = 0;
    while i < h {
        let row = i * w;
        let j = 0;
        while j < w {
            out[row + j] = a[row + j] * 3 + j;
            j = j + 1;
        }
        i = i + 1;
    }
}

// Reads at another value offset than the store: stays scalar, and the
// overlapping copy keeps scalar order.
fn copy_within(Vec<int> a, int from, int to, int n) {
    let j = 0;
    while j < n {
        a[to + j] = a[from + j] + 1;
        j = j + 1;
    }
}

fn copy_within_scalar(Vec<int> a, int from, int to, int n) {
    let j = 0;
    while j < n {
        a[to + j] = black_box(a[from + j] + 1);
        j = j + 1;
    }
}

test("inner loops over a[base + j]") {
    let a = ints(400);
    for w in [1, 7, 8, 9, 16, 19] {
        for h in [0, 1, 3, 5] {
            for base in [0, 1, 5] {
                assert!(rows(a, w, h, base) == rows_scalar(a, w, h, base));
            }
        }
    }
    let out: Vec<int> = Vec::new();
    let k = 0;
    while k < 400 {
        out.push(0);
        k = k + 1;
    }
    scale_rows(out, a, 19, 20);
    let i = 0;
    while i < 380 {
        assert!(out[i] == a[i] * 3 + i % 19);
        i = i + 1;
    }
    let x = ints(64);
    let y = ints(64);
    copy_within(x, 3, 5, 40);
    copy_within_scalar(y, 3, 5, 40);
    let m = 0;
    while m < 64 {
        assert!(x[m] == y[m]);
        m = m + 1;
    }
}

fn past_end_base() -> int {
    return rows(ints(40), 8, 2, 30);
}

fn negative_base() -> int {
    return rows(ints(40), 8, 1, -3);
}

test("a base past either end still panics") {
    assert!(panics(past_end_base));
    assert!(panics(negative_base));
}

// One loop that stores a map and folds a sum: the stores and the fold run
// in the same vector loop.
fn map_sum(Vec<int> out, Vec<int> a, int k) -> int {
    let s = 0;
    let i = 0;
    while i < a.len() {
        let v = a[i] * k + i;
        out[i] = v;
        s = s + v;
        i = i + 1;
    }
    return s;
}

fn map_sum_scalar(Vec<int> out, Vec<int> a, int k) -> int {
    let s = 0;
    let i = 0;
    while i < a.len() {
        let v = black_box(a[i] * k + i);
        out[i] = v;
        s = s + v;
        i = i + 1;
    }
    return s;
}

// Both reads come before the store, so the arrays may alias.
fn scale_dot(Vec<float> out, Vec<float> a, Vec<float> b) -> float {
    let s = 0.25;
    let i = 0;
    while i < a.len() {
        let x = a[i];
        let y = b[i];
        out[i] = x * 2.0;
        s = s + x * y;
        i = i + 1;
    }
    return s;
}

fn scale_dot_scalar(Vec<float> out, Vec<float> a, Vec<float> b) -> float {
    let s = 0.25;
    let i = 0;
    while i < a.len() {
        let x = a[i];
        let y = b[i];
        out[i] = black_box(x * 2.0);
        s = s + x * y;
        i = i + 1;
    }
    return s;
}

// The fold reads what the store just wrote: stays scalar.
fn double_then_sum(Vec<int> a) -> int {
    let s = 0;
    let i = 0;
    while i < a.len() {
        a[i] = a[i] * 2;
        s = s + a[i];
        i = i + 1;
    }
    return s;
}

test("loops that store a map and fold a sum") {
    for n in [0, 1, 7, 8, 9, 16, 17, 63, 64, 65] {
        let a = ints(n);
        let x = ints(n);
        let y = ints(n);
        assert!(map_sum(x, a, 3) == map_sum_scalar(y, a, 3));
        let i = 0;
        while i < n {
            assert!(x[i] == y[i]);
            i = i + 1;
        }
        let fa = floats(n);
        let fx = floats(n);
        let fy = floats(n);
        assert!(scale_dot(fx, fa, fa) == scale_dot_scalar(fy, fa, fa));
        let j = 0;
        while j < n {
            assert!(fx[j] == fy[j]);
            j = j + 1;
        }
        let d = ints(n);
        let e = ints(n);
        let expect = 0;
        let m = 0;
        while m < n {
            expect = expect + e[m] * 2;
            m = m + 1;
        }
        assert!(double_then_sum(d) == expect);
    }
}

// Lane casts: `as int` truncates toward zero (NaN is 0, out of range
// saturates) and `as float` rounds, in every lane as in the scalar loop.
fn trunc_map(Vec<float> out, Vec<float> a) {
    let i = 0;
    while i < a.len() {
        out[i] = (a[i] as int) as float;
        i = i + 1;
    }
}

fn trunc_map_scalar(Vec<float> out, Vec<float> a) {
    let i = 0;
    while i < a.len() {
        out[i] = black_box((a[i] as int) as float);
        i = i + 1;
    }
}

fn to_ints(Vec<int> out, Vec<float> a, float k) {
    let i = 0;
    while i < a.len() {
        out[i] = (a[i] * k) as int / 3;
        i = i + 1;
    }
}

fn to_ints_scalar(Vec<int> out, Vec<float> a, float k) {
    let i = 0;
    while i < a.len() {
        out[i] = black_box((a[i] * k) as int / 3);
        i = i + 1;
    }
}

fn halves(Vec<float> out, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        out[i] = (a[i] as float) * 0.5 + (i as float);
        i = i + 1;
    }
}

fn int_sum_of(Vec<float> a) -> int {
    let s = 0;
    let i = 0;
    while i < a.len() {
        s = s + (a[i] as int);
        i = i + 1;
    }
    return s;
}

fn int_sum_of_scalar(Vec<float> a) -> int {
    let s = 0;
    let i = 0;
    while i < a.len() {
        s = s + black_box(a[i] as int);
        i = i + 1;
    }
    return s;
}

fn same_float(float x, float y) -> bool {
    return x == y || (x != x && y != y);
}

test("lane casts between int and float") {
    for n in [0, 1, 7, 8, 9, 16, 17, 63, 64, 65] {
        let fa = floats(n);
        let k = 0;
        while k < n {
            fa[k] = fa[k] * 3.7 - 9.0;
            k = k + 1;
        }
        if n > 9 {
            fa[3] = 0.0 / 0.0;
            fa[5] = 9223372036854776000.0 * 1000.0;
            fa[6] = -9223372036854776000.0 * 1000.0;
            fa[7] = -0.75;
            fa[8] = 1.0 / 0.0;
        }
        let x = floats(n);
        let y = floats(n);
        trunc_map(x, fa);
        trunc_map_scalar(y, fa);
        let ix = ints(n);
        let iy = ints(n);
        to_ints(ix, fa, 2.5);
        to_ints_scalar(iy, fa, 2.5);
        let i = 0;
        while i < n {
            assert!(same_float(x[i], y[i]));
            assert!(ix[i] == iy[i]);
            i = i + 1;
        }
        let h = floats(n);
        let src = ints(n);
        halves(h, src);
        let j = 0;
        while j < n {
            assert!(h[j] == (src[j] as float) * 0.5 + (j as float));
            j = j + 1;
        }
        let fs = floats(n);
        assert!(int_sum_of(fs) == int_sum_of_scalar(fs));
    }
}

// Selects: an `if` whose arms cannot trap runs both arms in every lane
// and picks per lane, so it must give the scalar loop's elements, NaN and
// signed zeros included.
fn abs_map(Vec<float> out, Vec<float> a) {
    let i = 0;
    while i < a.len() {
        let x = a[i];
        if x < 0.0 {
            x = 0.0 - x;
        }
        out[i] = x;
        i = i + 1;
    }
}

fn abs_map_scalar(Vec<float> out, Vec<float> a) {
    let i = 0;
    while i < a.len() {
        let x = black_box(a[i]);
        if x < 0.0 {
            x = 0.0 - x;
        }
        out[i] = x;
        i = i + 1;
    }
}

fn clamp_or_double(Vec<float> out, Vec<float> a, float hi) {
    let i = 0;
    while i < a.len() {
        let x = a[i];
        if x > hi {
            out[i] = hi;
        } else {
            out[i] = x * 2.0;
        }
        i = i + 1;
    }
}

fn clamp_or_double_scalar(Vec<float> out, Vec<float> a, float hi) {
    let i = 0;
    while i < a.len() {
        let x = black_box(a[i]);
        if x > hi {
            out[i] = hi;
        } else {
            out[i] = x * 2.0;
        }
        i = i + 1;
    }
}

// One arm stores; the other side keeps the element.
fn cap(Vec<int> a, int hi) {
    let i = 0;
    while i < a.len() {
        if a[i] > hi {
            a[i] = hi;
        }
        i = i + 1;
    }
}

fn cap_scalar(Vec<int> a, int hi) {
    let i = 0;
    while i < a.len() {
        if black_box(a[i]) > hi {
            a[i] = hi;
        }
        i = i + 1;
    }
}

fn fold_upper(Vec<int> out, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        let c = a[i];
        if c >= 65 && c <= 90 {
            out[i] = c | 32;
        } else {
            out[i] = c;
        }
        i = i + 1;
    }
}

fn fold_upper_scalar(Vec<int> out, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        let c = black_box(a[i]);
        if c >= 65 && c <= 90 {
            out[i] = c | 32;
        } else {
            out[i] = c;
        }
        i = i + 1;
    }
}

// The `then` arm would overflow in the lanes that take `else`: it must
// not run there.
fn shift_small(Vec<int> out, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        let x = a[i];
        if x < 10 {
            out[i] = x + 9223372036854775000;
        } else {
            out[i] = x;
        }
        i = i + 1;
    }
}

test("selects in map loops") {
    for n in [0, 1, 7, 8, 9, 16, 17, 63, 64, 65] {
        let fa = floats(n);
        let k = 0;
        while k < n {
            fa[k] = fa[k] - 4.0;
            k = k + 1;
        }
        if n > 9 {
            fa[2] = 0.0 / 0.0;
            fa[3] = -0.0;
            fa[4] = -1.0 / 0.0;
        }
        let x = floats(n);
        let y = floats(n);
        abs_map(x, fa);
        abs_map_scalar(y, fa);
        let i = 0;
        while i < n {
            assert!(same_float(x[i], y[i]));
            assert!(1.0 / x[i] == 1.0 / y[i] || x[i] != x[i]);
            i = i + 1;
        }
        clamp_or_double(x, fa, 1.5);
        clamp_or_double_scalar(y, fa, 1.5);
        let j = 0;
        while j < n {
            assert!(same_float(x[j], y[j]));
            j = j + 1;
        }
        let ia = ints(n);
        let ib = ints(n);
        cap(ia, 100);
        cap_scalar(ib, 100);
        let letters: Vec<int> = Vec::new();
        let m = 0;
        while m < n {
            assert!(ia[m] == ib[m]);
            letters.push(40 + (m * 7) % 80);
            m = m + 1;
        }
        let la = ints(n);
        let lb = ints(n);
        fold_upper(la, letters);
        fold_upper_scalar(lb, letters);
        let big: Vec<int> = Vec::new();
        let q = 0;
        while q < n {
            assert!(la[q] == lb[q]);
            if q % 3 == 0 {
                big.push(q);
            } else {
                big.push(9223372036854775000 + q);
            }
            q = q + 1;
        }
        let sh = ints(n);
        shift_small(sh, big);
        let r = 0;
        while r < n {
            if big[r] < 10 {
                assert!(sh[r] == big[r] + 9223372036854775000);
            } else {
                assert!(sh[r] == big[r]);
            }
            r = r + 1;
        }
    }
}

// An `int` add in an arm runs in every lane when its operands' ranges
// keep it from overflowing: `(c as int) + 32` on a byte.
fn lower_bytes(Vec<byte> out, Vec<byte> b) {
    let i = 0;
    while i < b.len() {
        let c = b[i];
        if c >= "A" && c <= "Z" {
            out[i] = ((c as int) + 32) as byte;
        } else {
            out[i] = c;
        }
        i = i + 1;
    }
}

fn lower_bytes_scalar(Vec<byte> out, Vec<byte> b) {
    let i = 0;
    while i < b.len() {
        let c = black_box(b[i]);
        if c >= "A" && c <= "Z" {
            out[i] = ((c as int) + 32) as byte;
        } else {
            out[i] = c;
        }
        i = i + 1;
    }
}

test("byte maps with a ranged add in an arm") {
    for n in [0, 1, 7, 8, 9, 16, 17, 63, 64, 65, 300] {
        let src: Vec<byte> = Vec::new();
        let x: Vec<byte> = Vec::new();
        let y: Vec<byte> = Vec::new();
        let k = 0;
        while k < n {
            src.push(((k * 37 + 11) % 256) as byte);
            x.push(0 as byte);
            y.push(0 as byte);
            k = k + 1;
        }
        lower_bytes(x, src);
        lower_bytes_scalar(y, src);
        let i = 0;
        while i < n {
            assert!(x[i] == y[i]);
            i = i + 1;
        }
    }
}
