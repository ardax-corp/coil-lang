// `if … else …` is an expression: its value is the taken branch's trailing
// expression, and it binds as a primary, so `+ 1` applies to the whole `if`
// (as after a `match`).
// @output 1 2 11 4
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let c = true;
    let a = if c {
        1
    } else {
        2
    };
    let b = if !c {
        1
    } else {
        2
    };
    let d = if c {
        10
    } else {
        20
    } + 1;
    let e = if c {
        let k = 2;
        k * 2
    } else {
        0
    };
    let _ = write_all(stdout(), to_bytes(format("%i %i %i %i", a, b, d, e)));
}
