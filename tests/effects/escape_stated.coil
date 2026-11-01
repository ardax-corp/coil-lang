// Stated escape facts that hold compile and run: `#[fresh]` on a function
// whose result it builds itself, `#[noescape(..)]` on parameters a call
// only reads.
use tests::assert;

enum Tree {
    Leaf,
    Node(Tree, Tree),
}

#[fresh]
fn bottom_up(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(bottom_up(depth - 1), bottom_up(depth - 1));
}

#[noescape(t)]
fn item_check(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(left, right) => 1 + item_check(left) + item_check(right),
    };
}

#[noescape(a, b)]
#[fresh]
fn joined(string a, string b) -> string {
    return a + "-" + b;
}

fn main() {
    assert!(item_check(bottom_up(4)) == 31);
    assert!(joined("x", "y") == "x-y");
}
