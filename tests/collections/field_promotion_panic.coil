// A loop that keeps an object's fields in registers (`mir::promote_fields`)
// leaves them, when it panics, as the source's stores did by then. Each
// loop panics part way; a `defer` reads the fields afterwards.
// @output total 4611686018427387910 3\ncount 6 9223372036854775807\nindex 5 2\nfloat 4.5 2\ndone 6 3\nnone 0 0\n
use io::sync::println;
use string::format;
use thread::{join, spawn};

const BIG = 4611686018427387904;

const MAX = 9223372036854775807;

// One holder the panicking call and its `defer` both see.
shared class Acc {
    pub total: int,
    pub count: int,
    pub scale: float,
}

fn sum(Acc acc, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        acc.total = acc.total + a[i];
        acc.count = acc.count + 1;
        i = i + 1;
    }
}

// Reads one past the end on the last pass.
fn sum_next(Acc acc, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        acc.total = acc.total + a[i + 1];
        acc.count = acc.count + 1;
        i = i + 1;
    }
}

// The field is a float; the panic is an `int` overflow after its store.
fn grow(Acc acc, int n) -> int {
    let i = 0;
    let q = 0;
    while i < n {
        acc.scale = acc.scale + 1.5;
        q = q + BIG * i;
        acc.count = acc.count + 1;
        i = i + 1;
    }
    return q;
}

fn run_total() -> int {
    // `BIG + 1 + 2 + 3`, then `+ BIG` overflows before the fourth store.
    let acc = new Acc(BIG, 0, 0.0);
    defer use (acc) {
        let _ = println(format("total %i %i", acc.total, acc.count));
    }
    sum(acc, [1, 2, 3, BIG, 5]);
    return 0;
}

fn run_count() -> int {
    // The third `count + 1` overflows after the third total store.
    let acc = new Acc(0, MAX - 2, 0.0);
    defer use (acc) {
        let _ = println(format("count %i %i", acc.total, acc.count));
    }
    sum(acc, [1, 2, 3, 4, 5]);
    return 0;
}

fn run_index() -> int {
    let acc = new Acc(0, 0, 0.0);
    defer use (acc) {
        let _ = println(format("index %i %i", acc.total, acc.count));
    }
    sum_next(acc, [1, 2, 3]);
    return 0;
}

fn run_float() -> int {
    // `BIG * 2` overflows in the third pass, after its scale store.
    let acc = new Acc(0, 0, 0.0);
    defer use (acc) {
        let _ = println(format("float %v %i", acc.scale, acc.count));
    }
    return grow(acc, 5);
}

fn main() {
    for f in [run_total, run_count, run_index, run_float] {
        let t = match spawn(f) {
            Result::Ok(t) => t,
            Result::Err(_) => panic "spawn failed",
        };
        let _ = join(t);
    }
    let acc = new Acc(0, 0, 0.0);
    sum(acc, [1, 2, 3]);
    let _ = println(format("done %i %i", acc.total, acc.count));
    let none = new Acc(0, 0, 0.0);
    sum(none, []);
    let _ = println(format("none %i %i", none.total, none.count));
}
