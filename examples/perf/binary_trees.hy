// CPU: recursive alloc + walk (cross-lang fair bench).
// Binary trees checksum; max depth 10.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Tree {
    Leaf,
    Node(Tree, Tree),
}

#[max_depth(64)]
fn bottom_up(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(bottom_up(depth - 1), bottom_up(depth - 1));
}

#[max_depth(64)]
fn item_check(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(left, right) => 1 + item_check(left) + item_check(right),
    };
}

fn main() {
    let n = 10;
    let sum = item_check(bottom_up(n + 1));
    let long_lived = bottom_up(n);
    let depth = 4;
    while depth <= n {
        let iterations = 1 << (n - depth + 4);
        let i = 0;
        let c = 0;
        while i < iterations {
            c = c + item_check(bottom_up(depth));
            i = i + 1;
        }
        sum = sum + c;
        depth = depth + 2;
    }
    sum = sum + item_check(long_lived);
    write_all(stdout(), to_bytes(format("%i", sum)));
}
