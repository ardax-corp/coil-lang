// A vector loop whose block panics stores exactly what the scalar loop
// would have before the panic: the VM runs that block again one lane at a
// time (`machine::vm::vloop_lanes`). Each loop panics in the middle of
// the third 8-lane block; a `defer` counts the stores made.
// @output map 18\nmap scalar 18\nmixed 20\nmixed scalar 20\n
use io::sync::println;
use string::format;
use thread::{join, spawn};

const BIG = 4611686018427387904;

// One holder the panicking call and its `defer` both see.
shared class Out {
    pub b: Vec<int>,
}

fn input() -> Vec<int> {
    let a: Vec<int> = Vec::new();
    let i = 0;
    while i < 40 {
        a.push(if i == 18 || i == 19 { BIG } else { i });
        i = i + 1;
    }
    return a;
}

fn zeros() -> Vec<int> {
    let b: Vec<int> = Vec::new();
    let i = 0;
    while i < 40 {
        b.push(0);
        i = i + 1;
    }
    return b;
}

fn stored(Vec<int> b) -> int {
    let n = 0;
    let i = 0;
    while i < b.len() {
        if b[i] != 0 {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

// `a[18] * 2` overflows after 18 stores.
fn map(Vec<int> a, Vec<int> b) {
    let i = 0;
    while i < a.len() {
        b[i] = a[i] * 2 + 1;
        i = i + 1;
    }
}

fn map_scalar(Vec<int> a, Vec<int> b) {
    let i = 0;
    while i < a.len() {
        b[i] = black_box(a[i] * 2 + 1);
        i = i + 1;
    }
}

// The sum overflows at element 19, after its store: 20 stores.
fn mixed(Vec<int> a, Vec<int> b) -> int {
    let s = 0;
    let i = 0;
    while i < a.len() {
        let v = a[i] + 1;
        b[i] = v;
        s = s + v;
        i = i + 1;
    }
    return s;
}

fn mixed_scalar(Vec<int> a, Vec<int> b) -> int {
    let s = 0;
    let i = 0;
    while i < a.len() {
        let v = black_box(a[i] + 1);
        b[i] = v;
        s = s + v;
        i = i + 1;
    }
    return s;
}

fn run_map() -> int {
    let o = new Out(zeros());
    defer use (o) {
        let _ = println(format("map %i", stored(o.b)));
    }
    map(input(), o.b);
    return 0;
}

fn run_map_scalar() -> int {
    let o = new Out(zeros());
    defer use (o) {
        let _ = println(format("map scalar %i", stored(o.b)));
    }
    map_scalar(input(), o.b);
    return 0;
}

fn run_mixed() -> int {
    let o = new Out(zeros());
    defer use (o) {
        let _ = println(format("mixed %i", stored(o.b)));
    }
    return mixed(input(), o.b);
}

fn run_mixed_scalar() -> int {
    let o = new Out(zeros());
    defer use (o) {
        let _ = println(format("mixed scalar %i", stored(o.b)));
    }
    return mixed_scalar(input(), o.b);
}

fn main() {
    for f in [run_map, run_map_scalar, run_mixed, run_mixed_scalar] {
        let t = match spawn(f) {
            Result::Ok(t) => t,
            Result::Err(_) => panic "spawn failed",
        };
        let _ = join(t);
    }
}
