// A loop keeps a field in a register across a call only when the call's
// effect summary says it cannot touch that field (`mir::promote_fields`,
// locality step 3). Each callee recurses once so it stays a call. A
// callee that reads or writes the field, or calls a function value, sees
// the field as the source left it; a panic in a callee leaves the stores
// made before the call.
// @output weight 10 3\nread 4 10\nwrite 8 3\nvalue 3 3\npanic 3 2\n
use io::sync::println;
use string::format;
use thread::{join, spawn};

shared class Acc {
    pub total: int,
    pub count: int,
}

fn weight(int x) -> int {
    if x <= 0 {
        return 0;
    }
    return x % 7 + weight(x / 100);
}

fn peek(Acc a, int depth) -> int {
    if depth > 0 {
        return peek(a, depth - 1);
    }
    return a.total;
}

fn halve(Acc a, int depth) {
    if depth > 0 {
        halve(a, depth - 1);
        return;
    }
    a.total = a.total / 2;
}

fn apply(int x, int -> int f, int depth) -> int {
    if depth > 0 {
        return apply(x, f, depth - 1);
    }
    return f(x);
}

fn checked(int x, int depth) -> int {
    if depth > 0 {
        return checked(x, depth - 1);
    }
    if x == 13 {
        panic "thirteen";
    }
    return x;
}

fn sum_weights(Acc acc, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        acc.total = acc.total + weight(a[i]);
        acc.count = acc.count + 1;
        i = i + 1;
    }
}

fn sum_peeks(Acc acc, int n) {
    let i = 0;
    while i < n {
        acc.total = acc.total + 1;
        acc.count = acc.count + peek(acc, 1);
        i = i + 1;
    }
}

fn sum_halves(Acc acc, int n) {
    let i = 0;
    while i < n {
        acc.total = acc.total + 10;
        halve(acc, 1);
        acc.count = acc.count + 1;
        i = i + 1;
    }
}

fn sum_values(Acc acc, int n) {
    let f = fn (int v) use (acc) => v + acc.count;
    let i = 0;
    while i < n {
        acc.count = acc.count + 1;
        acc.total = apply(0, f, 1);
        i = i + 1;
    }
}

fn sum_checked(Acc acc, Vec<int> a) {
    let i = 0;
    while i < a.len() {
        acc.total = acc.total + checked(a[i], 1);
        acc.count = acc.count + 1;
        i = i + 1;
    }
}

fn run_panic() -> int {
    let acc = new Acc(0, 0);
    defer use (acc) {
        let _ = println(format("panic %i %i", acc.total, acc.count));
    }
    sum_checked(acc, [1, 2, 13, 4]);
    return 0;
}

fn main() {
    let w = new Acc(0, 0);
    sum_weights(w, [5, 123, 77]);
    let _ = println(format("weight %i %i", w.total, w.count));
    let r = new Acc(0, 0);
    sum_peeks(r, 4);
    let _ = println(format("read %i %i", r.total, r.count));
    let h = new Acc(0, 0);
    sum_halves(h, 3);
    let _ = println(format("write %i %i", h.total, h.count));
    let v = new Acc(0, 0);
    sum_values(v, 3);
    let _ = println(format("value %i %i", v.total, v.count));
    let t = match spawn(run_panic) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    let _ = join(t);
}
