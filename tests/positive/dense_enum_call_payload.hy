// An enum built from two recursive call results keeps both words as GC
// roots: the dense lowering used to type the first call's result as a
// scalar, so a collection during the second call freed that subtree.
enum Tree {
    Leaf,
    Node(Tree, Tree),
}

fn bottom_up(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    let a = bottom_up(depth - 1);
    let b = bottom_up(depth - 1);
    return Tree::Node(a, b);
}

fn direct(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(direct(depth - 1), direct(depth - 1));
}

fn count(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => count(l) + count(r),
    };
}

test("enum payloads from call results survive collection") {
    assert(count(bottom_up(16)) == 65536)?;
    assert(count(direct(16)) == 65536)?;
}
