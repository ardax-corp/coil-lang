// Expected: compile failure — `size` is an instance method of `Measurable`;
// `Box::size(..)` is not a spelling for it.
trait Measurable<T> {
    fn size(T x) -> int {}
}

class Box {
    pub w: int,
}

impl Measurable for Box {
    pub fn size(Box b) -> int {
        return b.w;
    }
}

fn main() {
    let n = Box::size(new Box(1));
}
