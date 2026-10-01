// `defer` runs on function exit (`return` / `return;`), LIFO.
// Outer locals must be listed in `use (…)` — same capture rule as lambdas.
//
// Output: enterleave,021,okd7,d99,55

use io::{stdout, write};
use string::{format, to_bytes};

fn with_cleanup() {
    defer {
        write(stdout(), to_bytes("leave"));
    }
    write(stdout(), to_bytes("enter"));
    return;
}

fn lifo() {
    defer {
        write(stdout(), to_bytes("1"));
    }
    defer {
        write(stdout(), to_bytes("2"));
    }
    write(stdout(), to_bytes("0"));
    return;
}

// Early `return` still runs deferred cleanup.
fn early_return(int n) -> int {
    defer {
        write(stdout(), to_bytes("d"));
    }
    if n == 0 {
        return 99;
    }
    write(stdout(), to_bytes("ok"));
    return n;
}

// Capture an outer local with `defer use (n)`.
fn capture_n(int n) -> int {
    defer use (n) {
        write(stdout(), to_bytes(format("%i", n)));
    }
    return n;
}

fn main() {
    with_cleanup();
    write(stdout(), to_bytes(","));
    lifo();
    write(stdout(), to_bytes(","));
    write(stdout(), to_bytes(format("%i", early_return(7))));
    write(stdout(), to_bytes(","));
    write(stdout(), to_bytes(format("%i", early_return(0))));
    write(stdout(), to_bytes(","));
    write(stdout(), to_bytes(format("%i", capture_n(5))));
    return;
}
