// Expected: compile failure — `Describe for Box<T: Describe>` needs
// `Describe<Point>`, and there is none.
class Box<T> {
    pub item: T,
}

class Point {
    pub x: int,
}

trait Describe<S> {
    fn describe(S x) -> string {}
}

impl Describe for Box<T: Describe> {
    pub fn describe(Box<T> b) -> string {
        return b.item.describe();
    }
}

fn main() {
    let s = new Box(new Point(1)).describe();
}
