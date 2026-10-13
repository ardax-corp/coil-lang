// MakeEnum one-pass payload build must keep mixed Value/Object order for match.
// Output: 11
use io::{stdout, write};
use string::{format, to_bytes};

enum Pair {
    Both(int, string),
}

fn describe(Pair p) -> int {
    return match p {
        Pair::Both(n, s) => n + len(s),
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i", describe(Pair::Both(7, "abcd")))));
}
