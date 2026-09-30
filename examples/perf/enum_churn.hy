// Local enums built on several paths and matched once: escape analysis
// turns them into a tag slot plus payload slots, so the loop allocates
// nothing (unit, one- and two-word variants).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Shape {
    Circle(int),
    Rect(int, int),
    Dot,
}

fn area(int i) -> int {
    let s = Shape::Dot;
    if i % 3 == 0 {
        s = Shape::Circle(i & 15);
    } else if i % 3 == 1 {
        s = Shape::Rect(2, i & 7);
    }
    return match s {
        Shape::Circle(r) => r * r * 3,
        Shape::Rect(w, h) => w * h,
        Shape::Dot => 0,
    };
}

fn direct(int i) -> int {
    let s = Shape::Rect(i & 3, i & 5);
    return match s {
        Shape::Circle(r) => r,
        Shape::Rect(w, h) => w * h,
        Shape::Dot => 0,
    };
}

fn main() {
    let t = 0;
    let i = 0;
    while i < 1000000 {
        t = t + area(i) + direct(i);
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", t)));
}
