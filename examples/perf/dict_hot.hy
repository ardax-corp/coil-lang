// CPU: dict field read/write pressure (GetField / SetField).
use io::sync::print;
use string::format;

fn main() {
    let d = { x: 0, y: 0 };
    let i = 0;
    while i < 2000 {
        d.x += 1;
        d.y += 2;
        i = i + 1;
    }
    let _ = print(format("%i", d.x + d.y));
}
