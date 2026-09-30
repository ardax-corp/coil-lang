// Async-first: block_on drives a coroutine to its completion value.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn greet() -> int {
    yield 1;
    return 2;
}

fn main() {
    let n = block_on(greet());
    write_all(stdout(), to_bytes(format("%i", n)));
}
