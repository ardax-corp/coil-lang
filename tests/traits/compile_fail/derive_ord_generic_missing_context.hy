// Expected: E0119 — `Box<T>` derives `Ord`, but `Plain` has no `Ord`, so `<` on `Box<Plain>` has no instance (#1186).
class Plain {
    pub v: int,
}

#[derive(Ord)]
class Box<T> {
    pub item: T,
}

fn main() {
    let b = new Box(new Plain(1)) < new Box(new Plain(2));
}
