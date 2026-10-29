// Self-recursive bodies on the dense tier keep call results on the operand
// stack where LIR does: each shape gives the same answer as plain code.
use tests::assert;

enum Tree {
    Leaf,
    Node(Tree, Tree),
}

fn build(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(build(depth - 1), build(depth - 1));
}

// `1 + f(l) + f(r)`: the first result takes `+ 1` while it is on top.
fn count(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => 1 + count(l) + count(r),
    };
}

// Non-commutative folds on either side of the call.
fn down(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return down(n - 1) - 3;
}

fn up(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return n - up(n - 1);
}

fn mixed(int n) -> int {
    if n <= 0 {
        return 1;
    }
    return (mixed(n - 1) - 1) - mixed(n - 2) * 2;
}

// `Some(f(n - 1) + 1)` and `Some(x + x)`: a computed word and a
// constant tag returned as a pair.
fn walk(int n) -> Option<int> {
    if n <= 0 {
        return Option::Some(0);
    }
    let x = match walk(n - 1) {
        Option::Some(v) => v,
        Option::None => 0,
    };
    return Option::Some(x + 1);
}

fn twice(int n, int bad) -> Option<int> {
    if bad == 1 {
        return Option::None;
    }
    return Option::Some(n + n);
}

fn depth(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(l, _) => 1 + depth(l),
    };
}

test("a self-call result folds before the next call") {
    assert!(count(build(0)) == 1);
    assert!(count(build(3)) == 15);
    assert!(count(build(6)) == 127);
}

test("a returned enum built from two self-calls") {
    assert!(depth(build(5)) == 5);
}

test("non-commutative ops beside a self-call") {
    assert!(down(4) == -12);
    assert!(up(1) == 1);
    assert!(up(4) == 2);
    assert!(mixed(0) == 1);
    assert!(mixed(3) == -2);
}

test("pair returns with a computed word") {
    assert!(walk(0) == Option::Some(0));
    assert!(walk(7) == Option::Some(7));
    assert!(twice(5, 0) == Option::Some(10));
    assert!(twice(5, 1) == Option::None);
}
