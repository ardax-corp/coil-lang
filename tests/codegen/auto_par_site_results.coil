// Results of recursive sites the auto-par pass may fork (binary `+` / `-`,
// an enum constructor, a call of the function itself, helper arms, a match
// arm) or must leave sequential: each equals the sequential answer.
use tests::assert;

enum Tree {
    Leaf,
    Node(Tree, Tree),
}

fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

#[max_depth(64)]
fn build(int n) -> Tree {
    if n <= 1 {
        return Tree::Leaf();
    }
    return Tree::Node(build(n - 1), build(n - 2));
}

#[max_depth(64)]
fn leaves(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => leaves(l) + leaves(r),
    };
}

fn diff(int n) -> int {
    if n <= 1 {
        return n;
    }
    return diff(n - 1) - diff(n - 2);
}

#[max_depth(4096)]
fn tak(int a, int b, int c) -> int {
    if b >= a {
        return c;
    }
    return tak(tak(a - 1, b, c), tak(b - 1, c, a), tak(c - 1, a, b));
}

fn pair_fib(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return fib(n) + fib(n - 1);
}

fn sq(int n) -> int {
    return n * n;
}

fn pair_sq(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return sq(n) + sq(n - 1);
}

#[max_depth(64)]
fn fibm(int n) -> int {
    if n <= 1 {
        return n;
    }
    return match n {
        default => fibm(n - 1) + fibm(n - 2),
    };
}

test("binary add above the grain floor") {
    assert!(fib(22) == 17711);
}

test("enum constructor combine") {
    // `build` shares fib's recurrence, so the leaf count is fib(22).
    assert!(leaves(build(21)) == 17711);
}

test("subtraction combine") {
    // d(n) = d(n-1) - d(n-2), d(0) = 0, d(1) = 1 has period 6.
    assert!(diff(22) == -1);
}

test("self-call combine, narrow and fair tak loads") {
    assert!(tak(21, 12, 6) == 12);
    assert!(tak(24, 22, 20) == 21);
    assert!(tak(18, 12, 6) == 7);
}

test("pure and trivial helper arms") {
    assert!(pair_fib(22) == 28657);
    assert!(pair_sq(22) == 925);
}

test("fork inside an irrefutable match arm") {
    assert!(fibm(22) == 17711);
}
