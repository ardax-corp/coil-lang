// Expected: E0119 — `Box` takes one type argument.
class Box<T> {
    pub item: T,
}

fn main() {
    let b: Box<int, int> = new Box(1);
}
