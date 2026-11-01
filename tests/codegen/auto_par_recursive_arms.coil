// Auto-par fork-join of pure recursive functions: an n-ary sum, let-bound
// arms, a `param + k` argument, a xor combine, a local argument and an enum
// constructor all join to the sequential result.
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

fn triple_fib(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return fib(n) + fib(n - 1) + fib(n - 2);
}

fn fib_let(int n) -> int {
    if n <= 1 {
        return n;
    }
    let a = fib_let(n - 1);
    let b = fib_let(n - 2);
    return a + b;
}

fn walk(int n, int k) -> int {
    if n <= 0 {
        return k;
    }
    return walk(n - 1, k) + walk(n - 1, k + 1);
}

#[max_depth(64)]
fn mix(int n) -> int {
    if n <= 1 {
        return n;
    }
    return mix(n - 1) ^ mix(n - 2);
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

test("an n-ary sum of helper calls") {
    // fib(22) + fib(21) + fib(20)
    assert!(triple_fib(22) == 35422);
}

test("let-bound arms") {
    assert!(fib_let(22) == 17711);
}

test("a param plus constant argument") {
    assert!(walk(16, 0) == 524288);
}

test("a xor combine") {
    assert!(mix(22) == 1);
}

test("a literal and a local argument") {
    assert!(fib(22) == 17711);
    let n = 22;
    assert!(fib(n) == 17711);
}

test("an enum constructor combine") {
    assert!(leaves(build(21)) == 17711);
}
