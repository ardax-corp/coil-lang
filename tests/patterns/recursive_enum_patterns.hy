// Patterns over a recursive enum: nested unit variants pick out leaves
// beneath a node, and a recursive walk uses payload bindings at each level.
use tests::assert;

enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}

fn leaf_count(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(_, l, r) => leaf_count(l) + leaf_count(r),
    };
}

fn shape(Tree t) -> string {
    return match t {
        Tree::Leaf => "empty",
        Tree::Node(_, Tree::Leaf, Tree::Leaf) => "single",
        Tree::Node(_, Tree::Node(_, _, _), Tree::Leaf) => "left",
        Tree::Node(_, Tree::Leaf, Tree::Node(_, _, _)) => "right",
        Tree::Node(_, _, _) => "both",
    };
}

fn max_value(Tree t) -> int {
    return match t {
        Tree::Leaf => -1,
        Tree::Node(v, l, r) => {
            let m = v;
            let a = max_value(l);
            let b = max_value(r);
            if a > m {
                m = a;
            }
            if b > m {
                m = b;
            }
            m
        },
    };
}

fn single(int v) -> Tree {
    return Tree::Node(v, Tree::Leaf, Tree::Leaf);
}

test("nested unit variants under a node") {
    assert!(shape(Tree::Leaf) == "empty");
    assert!(shape(single(1)) == "single");
    assert!(shape(Tree::Node(1, single(2), Tree::Leaf)) == "left");
    assert!(shape(Tree::Node(1, Tree::Leaf, single(2))) == "right");
    assert!(shape(Tree::Node(1, single(2), single(3))) == "both");
}

test("recursive walks") {
    let t = Tree::Node(5, Tree::Node(9, single(1), Tree::Leaf), single(7));
    assert!(leaf_count(Tree::Leaf) == 1);
    assert!(leaf_count(t) == 5);
    assert!(max_value(t) == 9);
    assert!(max_value(Tree::Leaf) == -1);
}
