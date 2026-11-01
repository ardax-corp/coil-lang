// A generic function value bound with `let` in an inner block shadows only
// inside that block; the outer local of the same name is unchanged after it.
// @output 423
use io::{stdout, write};
use string::{format, to_bytes};

trait Describable<T> {
    fn describe_val(T x) -> int {}
}

impl Describable for int {
    pub fn describe_val(int x) -> int {
        return x + 1;
    }
}

fn show<T: Describable>(T x) -> int {
    return describe_val(x);
}

fn capture_show<T: Describable>(T _w) {
    return show;
}

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let f = add;
    {
        let f = capture_show(0);
        let _ = write(stdout(), to_bytes(format("%i", f(41))));
    }
    let _ = write(stdout(), to_bytes(format("%i", f(1, 2))));
}
