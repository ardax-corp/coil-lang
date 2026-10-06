// Ground instances of generic user enums: built, returned in two words,
// matched with their parameters bound, and nested.
enum Box<T> {
    Empty,
    Full(T),
}

enum Tree<T> {
    Leaf(T),
    Node(Tree<T>, Tree<T>),
}

fn unwrap(Box<int> b) -> int {
    return match b {
        Box::Empty => 0,
        Box::Full(v) => v,
    };
}

fn make(int x) -> Box<int> {
    return Box::Full(x);
}

fn name(Box<string> b) -> string {
    return match b {
        Box::Empty => "none",
        Box::Full(s) => s,
    };
}

fn sum(Tree<int> t) -> int {
    return match t {
        Tree::Leaf(v) => v,
        Tree::Node(l, r) => sum(l) + sum(r),
    };
}

test("box of int") {
    assert(unwrap(make(7)) == 7)?;
    assert(unwrap(Box::Empty) == 0)?;
}

test("box of string") {
    assert(name(Box::Full("x")) == "x")?;
    assert(name(Box::Empty) == "none")?;
}

test("recursive tree") {
    let t: Tree<int> = Tree::Node(Tree::Leaf(1), Tree::Node(Tree::Leaf(2), Tree::Leaf(3)));
    assert(sum(t) == 6)?;
}
