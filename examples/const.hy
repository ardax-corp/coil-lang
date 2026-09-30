use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    const answer = 42;
    write_all(stdout(), to_bytes(format("%i", answer)));
    const greeting = "hi";
    write_all(stdout(), to_bytes(format("%s", greeting)));
}
