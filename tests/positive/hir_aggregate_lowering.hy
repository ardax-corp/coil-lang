// Bodies the HIR lowering covers since tuples, arrays and Vec: literals,
// indexing (with the Euclidean fix-up for `x % m`), index writes and
// `op=`, `len`, and the builtin Vec constructor and methods. The
// language harness also runs under `--hir`, so each case pins both
// codegens.
fn sum_arr([int] xs) -> int {
    let total = 0;
    let i = 0;
    while i < len(xs) {
        total = total + xs[i];
        i = i + 1;
    }
    return total;
}

fn fill([int] xs, int v) {
    let i = 0;
    while i < xs.len() {
        xs[i] = v + i;
        xs[i] += 1;
        i = i + 1;
    }
}

fn pair_sum((int, int, int) t) -> int {
    return t[0] + t[1] * t[2];
}

fn ring([int] xs, int k) -> int {
    return xs[k % 3] + xs[(k + 1) % 3];
}

fn make_tuple(int a) -> (int, string, int) {
    return (a, "x", a * 2);
}

fn heap_arr(int a) -> [int] {
    return [a, a + 1, a + 2];
}

fn nested(int a) -> int {
    let t = make_tuple(a);
    let xs = heap_arr(a);
    return t[0] + t[2] + xs[1] + len(xs);
}

fn staged([int] xs, int a) -> int {
    return xs[sum_arr(xs) % 3] + a;
}

fn build(int n) -> Vec<int> {
    let v = Vec::new();
    let i = 0;
    while i < n {
        v.push(i * 2);
        i = i + 1;
    }
    return v;
}

fn total(Vec<int> v) -> int {
    let s = 0;
    let i = 0;
    while i < v.len() {
        s = s + v[i];
        i = i + 1;
    }
    return s + len(v);
}

fn vec_ops(int n) -> int {
    let v = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        v.push(total(build(i)));
        i = i + 1;
    }
    v.insert(0, 100);
    let c = v.len();
    v.clear();
    return c * 1000 + v.len();
}

test("Vec constructors and methods") {
    assert(total(build(5)) == 25)?;
    assert(vec_ops(3) == 4000)?;
}

test("arrays and tuples") {
    let xs = heap_arr(1);
    assert(sum_arr(xs) == 6)?;
    fill(xs, 10);
    assert(sum_arr(xs) == 36)?;
    assert(pair_sum((1, 2, 3)) == 7)?;
    assert(ring(xs, -1) == 24)?;
    assert(nested(2) == 12)?;
    assert(staged(heap_arr(1), 5) == 6)?;
}
