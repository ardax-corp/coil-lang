use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn sum(int... xs) -> int {
    let n = len(xs);
    let total = 0;
    let i = 0;
    while i < n {
        total = total + xs[i];
        i = i + 1;
    }
    return total;
}

fn greet(string name, string... extras) -> string {
    let out = name;
    let n = len(extras);
    let i = 0;
    while i < n {
        out = out + extras[i];
        i = i + 1;
    }
    return out;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", sum(1, 2, 3))));
    write_all(stdout(), to_bytes(format("%i", sum())));
    write_all(stdout(), to_bytes(format("%s", greet(name: "Hi", "!", "?"))));
}
