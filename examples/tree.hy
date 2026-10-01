// examples/tree.hy — recursive enum to verify isorecursive encoding
//
// Output: 6

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}

#[max_depth(64)]
fn sum_tree(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(v, left, right) => v + sum_tree(left) + sum_tree(right),
    };
}

fn main() {
    write_all(
        stdout(),
        to_bytes(
            format(
                "%i",
                sum_tree(
                    Tree::Node(
                        1,
                        Tree::Node(2, Tree::Leaf(), Tree::Leaf()),
                        Tree::Node(3, Tree::Leaf(), Tree::Leaf()),
                    ),
                ),
            ),
        ),
    );
}
