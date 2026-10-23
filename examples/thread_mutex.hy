// Threads: a shared counter behind a `mutex`.
//
// @output 2

use thread::{join, mutex, spawn, with_lock};
use io::sync::print;
use string::format;

fn bump(Mutex m) {
    with_lock(m, fn (int n) => (n + 1, 0))?;
}

fn main() {
    let m = mutex(0)?;
    let t1 = spawn(bump, m)?;
    let t2 = spawn(bump, m)?;
    (join(t1)?)?;
    (join(t2)?)?;
    let n = with_lock(m, fn (int x) => (x, x))?;
    let _ = print(format("%i", n));
}
