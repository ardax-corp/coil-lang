// Heap-payload Result: helper RETURNS Result<Node, Node>.
// Both variants are class instances (never immediates) so Ok is the
// aligned pointer and Err is pointer | 1 — no ObjEnum per call.
// Explicit return poisons #278 frame-local unboxing (local_escape.rs).
// Integer counterpart: examples/perf/result_int_churn.hy.
// ITERS=10000000 (each step adds 13); checksum 130000000.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Node {
    pub v: int,
}

fn step(int i) -> Result<Node, Node> {
    if i % 2 == 0 {
        return Result::Ok(new Node(13));
    }
    return Result::Err(new Node(13));
}

fn main() {
    let iters = 10000000;
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match step(i) {
            Result::Ok(n) => n.v,
            Result::Err(e) => e.v,
        };
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
