// Expected: E0102 or E0113 — `resume h with v` needs `v` of the coroutine's
// send type (`int` here, from `n + 1`).
gen fn adder() {
    let n = yield 0;
    yield n + 1;
}

fn main() {
    let h = adder();
    let _ = resume h;
    let _ = resume h with "one";
}
