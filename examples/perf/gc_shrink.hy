// GC: a large short-lived peak, then a long run with a small live set.
// Empty slab chunks idle for a whole release window go back to the OS
// (`Slab::release_idle_chunks`); RSS after the peak should drop. Checksum is
// the phase-2 work.
use io::{stdout};
use io::sync::{write_all};
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

fn sum(Option<Node> list) -> int {
    let s = 0;
    let cur = list;
    let done = false;
    while !done {
        match cur {
            Option::Some(n) => {
                s = s + n.v;
                cur = n.next;
            },
            Option::None => {
                done = true;
            },
        };
    }
    return s;
}

fn peak() -> int {
    return sum(build(400000));
}

fn main() {
    let total = peak();
    // Phase 2: small tuples churn through many collections.
    let round = 0;
    while round < 3000000 {
        let t = (round, round + 1);
        total = total + t[1] - t[0];
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
