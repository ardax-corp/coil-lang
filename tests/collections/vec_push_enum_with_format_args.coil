// COI-16: enum `Construct` with nested `format` must stage the push receiver
// (len preserved). Nested format-inside-Construct payload correctness is a
// separate clobber (still open); this only guards the Vec::push drop.
// push with Construct(format,…) must not drop the vec
// @output 2
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Row {
    Pair(string, string),
}

fn main() {
    let rows = Vec::new();
    rows.push(Row::Pair(format("a=%s", "1"), format("b=%s", "2")));
    rows.push(Row::Pair(format("c=%s", "3"), format("d=%s", "4")));
    let _ = write_all(stdout(), to_bytes(format("%i", rows.len())));
}
