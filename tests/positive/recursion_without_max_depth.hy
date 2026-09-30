// Recursion the depth analysis cannot bound needs no attribute: the VM grows
// its operand stack whenever a frame opens without room.
fn noise(int n) -> int {
    return n;
}

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn depth(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return 1 + depth(n - 1);
}

fn ping(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return pong(n - 1) + 1;
}

fn pong(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return ping(n - 1) + 1;
}

enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}

fn sum_tree(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(v, left, right) => v + sum_tree(left) + sum_tree(right),
    };
}

// A hint smaller than the real depth is not a promise the VM relies on.
#[max_depth(4)]
fn countdown(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return 1 + countdown(n - 1);
}

test("dynamic entry") {
    assert(fib(noise(10)) == 55)?;
}

test("deeper than the initial stack") {
    assert(depth(noise(20000)) == 20000)?;
}

test("non-tail mutual recursion") {
    assert(ping(noise(10001)) == 10001)?;
}

test("recursive enum walk") {
    let t = Tree::Node(
        1,
        Tree::Node(2, Tree::Leaf(), Tree::Leaf()),
        Tree::Node(3, Tree::Leaf(), Tree::Leaf()),
    );
    assert(sum_tree(t) == 6)?;
}

test("max_depth below the real depth") {
    assert(countdown(noise(5000)) == 5000)?;
}
