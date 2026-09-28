// GC: a large live Vec<int> through heavy allocation churn. Marking scans the
// vector once, finds no references, and skips it until it is written again
// (ObjArray::may_hold_refs).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn fill(int n) -> Vec<int> {
    let v: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    return v;
}

class Node {
    pub v: int,
    pub next: Option<Node>,
}

// Real short-lived garbage (escape analysis cannot remove it): a list that
// grows for 64 iterations, then is dropped.
fn churn(Option<Node> junk, int round) -> Option<Node> {
    if round % 64 == 0 {
        return Option::None;
    }
    return Option::Some(new Node(round, junk));
}

fn main() {
    let big = fill(2000000);
    let total = 0;
    let junk: Option<Node> = Option::None;
    let round = 0;
    while round < 2000000 {
        junk = churn(junk, round);
        total = total + 1;
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i %i", total, len(big))));
}
