// Variadic parameters (`int... xs`).
//
// @output 60Hi!?

use io::sync::print;
use string::format;

fn sum(int... xs) -> int {
    let n = xs.len();
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
    let n = extras.len();
    let i = 0;
    while i < n {
        out = out + extras[i];
        i = i + 1;
    }
    return out;
}

fn main() {
    let _ = print(format("%i", sum(1, 2, 3)));
    let _ = print(format("%i", sum()));
    let _ = print(format("%s", greet(name: "Hi", "!", "?")));
}
