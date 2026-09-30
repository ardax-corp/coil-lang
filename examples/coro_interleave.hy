// Two handles from the SAME parameterized async fn, interleaved,
// with `resume` used inline directly as a `print` argument.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn counter(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}

fn main() {
    let a = counter(10);
    let b = counter(100);

    // Interleave resumes: a, b, b, a, a, b
    write_all(stdout(), to_bytes(format("%i,", resume a)));
    write_all(stdout(), to_bytes(format("%i,", resume b)));
    write_all(stdout(), to_bytes(format("%i,", resume b)));
    write_all(stdout(), to_bytes(format("%i,", resume a)));
    write_all(stdout(), to_bytes(format("%i,", resume a)));
    write_all(stdout(), to_bytes(format("%i", resume b)));
}
