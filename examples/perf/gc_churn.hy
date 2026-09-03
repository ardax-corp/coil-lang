// GC tracking: alloc + forced collect (not a CPU fuse target).
// N=125000, ROUNDS=8 (~1e6 Nodes); live set drops between rounds.
use gc::{collect};
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn build_list(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = n - 1;
    while i >= 0 {
        let node = new Node(i, Option::None);
        node.next = head;
        head = Option::Some(node);
        i = i - 1;
    }
    return head;
}

fn checksum_walk(Option<Node> cur) -> int {
    let acc = 0;
    let go = true;
    while go {
        acc = acc + match cur {
            Option::Some(node) => {
                cur = node.next;
                node.v
            },
            Option::None => {
                go = false;
                0
            },
        };
    }
    return acc;
}

fn churn_round(int n) -> int {
    let head = build_list(n);
    return checksum_walk(head);
}

fn main() {
    let n = 125000;
    let rounds = 8;
    let acc = 0;
    let r = 0;
    while r < rounds {
        acc = acc + churn_round(n);
        collect();
        r = r + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
