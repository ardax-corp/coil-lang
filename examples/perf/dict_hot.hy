// CPU: dict field read/write pressure (GetField / SetField).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let d = { x: 0, y: 0 };
    let i = 0;
    while i < 2000 {
        d.x += 1;
        d.y += 2;
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", d.x + d.y)));
}
