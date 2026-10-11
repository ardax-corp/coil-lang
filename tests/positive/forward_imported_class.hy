// A function declared after its caller may take and return an imported
// class: its forward signature resolves `Rec` through the `use` (#888).

use forward_rec::{Rec, make};

fn twice(int n) -> int {
    let r = make(n);
    return later(r).n;
}

fn later(Rec r) -> Rec {
    return make(r.n * 2);
}

test("later fn takes and returns an imported class") {
    assert(twice(3) == 6)?;
    assert(later(make(5)).n == 10)?;
}
