// Memory: a loop rebuilds a large list into the same local. The previous
// round's list is dead once the local is about to be overwritten; a frame
// map that still lists the local keeps both lists alive at peak.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn build(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = 0;
    while i < n {
        head = Option::Some(new Node(i, head));
        i = i + 1;
    }
    return head;
}

fn len_of(Option<Node> head) -> int {
    let n = 0;
    let cur = head;
    let go = true;
    while go {
        match cur {
            Option::Some(node) => {
                n = n + 1;
                cur = node.next;
            },
            Option::None => {
                go = false;
            },
        }
    }
    return n;
}

fn main() {
    let total = 0;
    let list = build(1);
    let round = 0;
    while round < 12 {
        list = build(300000);
        total = total + len_of(list);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
