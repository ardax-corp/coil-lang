// Memory: a static registry of 100k live nodes (a list head and a Vec), half
// rebuilt each round with churn in between. Statics carry compile-time word
// kinds, so the registry roots are precise (a moving collector rewrites them)
// and the scalar counters are no roots at all.
use gc::{collect};
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

static let head: Option<Node> = Option::None;
static let nodes: Vec<Node> = Vec::new();
static let total: int = 0;

fn fill(int n) {
    let i = 0;
    while i < n {
        let node = new Node(i, head);
        head = Option::Some(node);
        nodes.push(node);
        i = i + 1;
    }
}

// Keep every other node in a fresh Vec; the old Vec and the dropped half die.
fn thin() {
    let kept: Vec<Node> = Vec::new();
    let j = 0;
    while j < len(nodes) {
        if j % 2 == 0 {
            kept.push(nodes[j]);
        }
        j = j + 1;
    }
    nodes = kept;
    head = Option::None;
}

fn settle() {
    collect();
}

fn main() {
    fill(100000);
    thin();
    settle();
    let round = 0;
    while round < 32 {
        let junk = 0;
        while junk < 20000 {
            let t = new Node(junk, Option::None);
            junk = junk + t.v - t.v + 1;
        }
        let i = 0;
        while i < len(nodes) {
            total = total + nodes[i].v % 7;
            i = i + 1;
        }
        settle();
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
