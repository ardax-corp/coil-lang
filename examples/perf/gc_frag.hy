// Memory: a long-lived list thinned in place leaves survivors scattered over
// half-empty slab chunks. Unmapping empty chunks cannot return that memory;
// evacuation (`gc-compact`) can. Walks the survivors after churn.
use gc::{collect};
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn build(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = n - 1;
    while i >= 0 {
        head = Option::Some(new Node(i, head));
        i = i - 1;
    }
    return head;
}

// Unlink every other node, and three of every four in a second pass.
fn thin(Option<Node> head, int keep) {
    let cur = head;
    let go = true;
    while go {
        match cur {
            Option::Some(node) => {
                let k = 1;
                let next = node.next;
                while k < keep {
                    next = match next {
                        Option::Some(skip) => skip.next,
                        Option::None => Option::None,
                    };
                    k = k + 1;
                }
                node.next = next;
                cur = next;
            },
            Option::None => {
                go = false;
            },
        };
    }
}

fn sum(Option<Node> head) -> int {
    let total = 0;
    let cur = head;
    let go = true;
    while go {
        match cur {
            Option::Some(node) => {
                total = total + node.v;
                cur = node.next;
            },
            Option::None => {
                go = false;
            },
        };
    }
    return total;
}

fn churn(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        let t = new Node(i, Option::None);
        acc = acc + t.v % 3;
        i = i + 1;
    }
    return acc;
}

fn main() {
    let head = build(400000);
    thin(head, 4);
    collect();
    let total = 0;
    let round = 0;
    while round < 24 {
        total = total + churn(20000) + sum(head);
        collect();
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
