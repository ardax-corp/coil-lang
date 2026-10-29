// @expected E0119 — `new Sorted(…)` at `P` needs `Ord<P>`: the bound on `class Sorted<T: Ord>` holds at every construction.
class Sorted<T: Ord> {
    pub items: Vec<T>,
}

class P {}

fn main() {
    let s = new Sorted(Vec::from([new P()]));
}
