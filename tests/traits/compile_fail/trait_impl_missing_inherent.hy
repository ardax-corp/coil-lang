// Expected: E0101 — a trait impl body calls `m.iter()`, but `Box` declares no
// inherent `iter`.
class Box {
    pub v: int,
}

class BoxIter {
    pub i: int,
}

impl IntoIterator for Box {
    type Item = int;
    type IntoIter = BoxIter;

    pub fn into_iter(Box m) -> BoxIter {
        return m.iter();
    }
}

impl Iterator for BoxIter {
    type Item = int;

    pub fn next(BoxIter it) -> Option<int> {
        return Option::None;
    }
}

fn main() {
    let b = new Box(1);
}
