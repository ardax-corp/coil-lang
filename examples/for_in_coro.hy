use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn counter() {
    yield 0;
    yield 1;
    yield 2;
    return 99;
}

async fn early() {
    yield 10;
    yield 20;
    yield 30;
}

fn main() {
    for x in counter() {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
    for y in early() {
        if y == 20 {
            break;
        }
        write_all(stdout(), to_bytes(format("%i", y)));
    }
}
