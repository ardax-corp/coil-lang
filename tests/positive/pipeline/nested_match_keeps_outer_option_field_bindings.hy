// Output: 42,100,6
use io::{stdout, write};
use string::{format, to_bytes};

class BoxInt {
    pub opt: Option<int>,
}

class Node {
    pub val: int,
    pub left: Option<Node>,
}

fn nested_boxed(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => match b.opt {
            Option::Some(v2) => v + v2,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn nested_shadow(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => match Option::Some(100) {
            Option::Some(v) => v,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn nested_niche(Node n) -> int {
    return match n.left {
        Option::Some(child) => match n.left {
            Option::Some(child2) => child.val + child2.val,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn main() {
    let b = new BoxInt(Option::Some(21));
    let leaf = new Node(3, Option::None);
    let root = new Node(1, Option::Some(leaf));
    write(
        stdout(),
        to_bytes(format("%i,%i,%i", nested_boxed(b), nested_shadow(b), nested_niche(root))),
    );
}
