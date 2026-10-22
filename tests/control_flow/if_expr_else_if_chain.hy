// An `else if` chain as a value: each branch's trailing expression, joined
// to one type (`string` here).
// @output neg zero small big
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn name(int n) -> string {
    return if n < 0 {
        "neg"
    } else if n == 0 {
        "zero"
    } else if n < 10 {
        "small"
    } else {
        "big"
    };
}

fn main() {
    let s = name(-3) + " " + name(0) + " " + name(4) + " " + name(40);
    let _ = write_all(stdout(), to_bytes(s));
}
