// @expected E0119 — the inherent `iter` declared after the trait impl takes no
// arguments, so `m.iter(1)` is a call with too many.
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
        return m.iter(1);
    }
}

impl Box {
    pub fn iter() -> BoxIter {
        return new BoxIter(self.v);
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
