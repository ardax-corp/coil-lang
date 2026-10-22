// A generic `#[derive(Ord)]` class compares with `<` / `>=` at an element
// type that has `Ord`, including a nested derived class (#1186).
// @output true false true true
use io::{stdout, write};
use string::{format, to_bytes};

#[derive(Eq, Ord)]
class Box<T> {
    pub item: T,
}

fn main() {
    let a = new Box(1) < new Box(2);
    let b = new Box(3) < new Box(2);
    let c = new Box(new Box(5)) >= new Box(new Box(4));
    let d = new Box(1.5) <= new Box(2.5);
    let _ = write(stdout(), to_bytes(format("%v %v %v %v\n", a, b, c, d)));
}
