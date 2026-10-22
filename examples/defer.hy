// `defer` runs on function exit (`return` / `return;`), LIFO.
// Outer locals must be listed in `use (…)` — same capture rule as lambdas.
//
// @output enterleave,021,okd7,d99,55

use io::{stdout, write};
use string::{format, to_bytes};

fn with_cleanup() {
    defer {
        let _ = write(stdout(), to_bytes("leave"));
    }
    let _ = write(stdout(), to_bytes("enter"));
    return;
}

fn lifo() {
    defer {
        let _ = write(stdout(), to_bytes("1"));
    }
    defer {
        let _ = write(stdout(), to_bytes("2"));
    }
    let _ = write(stdout(), to_bytes("0"));
    return;
}

// Early `return` still runs deferred cleanup.
fn early_return(int n) -> int {
    defer {
        let _ = write(stdout(), to_bytes("d"));
    }
    if n == 0 {
        return 99;
    }
    let _ = write(stdout(), to_bytes("ok"));
    return n;
}

// Capture an outer local with `defer use (n)`.
fn capture_n(int n) -> int {
    defer use (n) {
        let _ = write(stdout(), to_bytes(format("%i", n)));
    }
    return n;
}

fn main() {
    with_cleanup();
    let _ = write(stdout(), to_bytes(","));
    lifo();
    let _ = write(stdout(), to_bytes(","));
    let _ = write(stdout(), to_bytes(format("%i", early_return(7))));
    let _ = write(stdout(), to_bytes(","));
    let _ = write(stdout(), to_bytes(format("%i", early_return(0))));
    let _ = write(stdout(), to_bytes(","));
    let _ = write(stdout(), to_bytes(format("%i", capture_n(5))));
    return;
}
