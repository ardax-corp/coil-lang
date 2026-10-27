// Regions (escape-memory step 5): per round, a linked list whose nodes a
// vector also holds (shared, so early freeing cannot take it), summed both
// ways. `coil run region_rounds.hyc -- region|plain WIDTH ROUNDS`; `region`
// wraps each round in a region.
use io::sync::print;
use string::format;
use env::args;

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn round_of(int n, int seed) -> int {
    let all: Vec<Node> = Vec::new();
    let head: Option<Node> = Option::None;
    let i = 0;
    while i < n {
        let node = new Node(i + seed, head);
        all.push(node);
        head = Option::Some(node);
        i += 1;
    }
    let s = 0;
    for x in all {
        s += x.v;
    }
    let cur = head;
    while let Option::Some(x) = cur {
        s -= x.v;
        cur = x.next;
    }
    return s + all.len();
}

fn int_of(string s) -> int {
    let n = 0;
    let i = 0;
    while i < s.len() {
        n = n * 10 + (s[i] as int) - 48;
        i += 1;
    }
    return n;
}

fn main() {
    let mode = "plain";
    let width = 1000;
    let rounds = 1000;
    if let Result::Ok(a) = args() {
        if a.len() > 3 {
            mode = a[1];
            width = int_of(a[2]);
            rounds = int_of(a[3]);
        }
    }
    let sum = 0;
    let r = 0;
    if mode == "region" {
        while r < rounds {
            sum += region {
                round_of(width, r)
            };
            r += 1;
        }
    } else {
        while r < rounds {
            sum += round_of(width, r);
            r += 1;
        }
    }
    let _ = print(format("%i\n", sum));
}
