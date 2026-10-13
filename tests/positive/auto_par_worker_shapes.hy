// Auto-par fork-join workers are built as HIR and lowered like any other
// body: each combine shape (binary op, n-ary sum, xor, a call of the
// function itself, a helper call, an enum payload) forks and joins with
// the sequential result, guards and hop budgets included.
use tests::assert;

enum Tree {
    Leaf,
    Node(Tree, Tree),
}

fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn trib(int n) -> int {
    if n < 3 {
        return 1;
    }
    return trib(n - 1) + trib(n - 2) + trib(n - 3);
}

fn mix(int n) -> int {
    if n < 2 {
        return n * 7 + 1;
    }
    return mix(n - 1) ^ mix(n - 2);
}

fn pick(int a, int b) -> int {
    if a > b {
        return a - b;
    }
    return b - a;
}

fn spread(int n) -> int {
    if n < 2 {
        return n;
    }
    return pick(spread(n - 1), spread(n - 2));
}

fn tak(int x, int y, int z) -> int {
    if y >= x {
        return z;
    }
    return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y));
}

fn grow(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(grow(depth - 1), grow(depth - 1));
}

fn count(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => 1 + count(l) + count(r),
    };
}

fn fib_seq(int n) -> int {
    let a = 0;
    let b = 1;
    let i = 0;
    while i < n {
        let t = a + b;
        a = b;
        b = t;
        i = i + 1;
    }
    return a;
}

// Literal and computed calls from function bodies demand the workers.
fn run_fib(int n) -> int {
    return fib(n);
}

fn run_trib() -> int {
    return trib(22);
}

fn run_mix() -> int {
    return mix(24);
}

fn run_spread() -> int {
    return spread(24);
}

fn run_tak() -> int {
    return tak(18, 12, 6);
}

fn run_grow() -> Tree {
    return grow(14);
}

fn spread_seq(int n) -> int {
    let a = 0;
    let b = 1;
    let i = 1;
    while i < n {
        let t = pick(b, a);
        a = b;
        b = t;
        i = i + 1;
    }
    if n == 0 {
        return 0;
    }
    return b;
}

fn mix_seq(int n) -> int {
    let a = 1;
    let b = 8;
    let i = 1;
    while i < n {
        let t = b ^ a;
        a = b;
        b = t;
        i = i + 1;
    }
    if n == 0 {
        return a;
    }
    return b;
}

test("binary and n-ary combines") {
    assert!(run_fib(25) == fib_seq(25));
    assert!(run_fib(4) == 3);
    assert!(run_trib() == 289329);
}

test("xor and helper-call combines") {
    assert!(run_mix() == mix_seq(24));
    assert!(run_spread() == spread_seq(24));
}

test("self-call and enum combines") {
    assert!(run_tak() == 7);
    assert!(count(run_grow()) == 32767);
}
