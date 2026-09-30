use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

fn main() {
    let h = counter();
    write_all(stdout(), to_bytes(format("%i", resume h)));
    write_all(stdout(), to_bytes(format("%i", resume h)));
    write_all(stdout(), to_bytes(format("%i", resume h)));
}
