use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
attr log<T>(fn(...args) -> T target, string message, ...args) -> T {
    write_all(stdout(), to_bytes(format("%s", message)));
    return target(...args);
}

#[log(message = "Point ctor")]
class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new Point(5, 12);
    write_all(stdout(), to_bytes(format("%i", p.x)));
    write_all(stdout(), to_bytes(format("%i", p.y)));
}
