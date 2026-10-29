// A derive reads the fields it is derived on whether or not they are `pub`:
// the generated instance belongs to the type (#2084). A hand-written
// `impl Trait for C` in C's own module sees its private members for the same
// reason; one in another module does not
// (`tests/modules/compile_fail/trait_impl_private_field_other_module.hy`).
use tests::assert;

#[derive(Show, Eq, Ord, Hash)]
class C {
    n: int,
    label: string,
}

#[derive(Show, Eq)]
class Wrap<T> {
    inner: T,
}

#[derive(Show, Eq, Ord, Hash)]
enum Sh {
    Dot,
    Rect { w: int, h: int },
}

trait Peek<T> {
    fn peek(T self) -> int {}
}

impl Peek for C {
    pub fn peek(C c) -> int {
        return c.n;
    }
}

class Powers {
    cur: int,
}

impl Iterator for Powers {
    type Item = int;
    pub fn next(Powers it) -> Option<int> {
        if it.cur > 8 {
            return Option::None;
        }
        it.cur = it.cur * 2;
        return Option::Some(it.cur);
    }
}

test("derive Show / Eq on private fields") {
    let c = new C(1, "a");
    assert!(c.show() == "C { n: 1, label: a }", c.show());
    assert!(c == new C(1, "a"), "eq");
    assert!(c != new C(2, "a"), "ne");
}

test("derive Ord / Hash on private fields") {
    assert!(new C(1, "a") < new C(1, "b"));
    assert!(new C(2, "a") > new C(1, "z"));
    assert!(new C(1, "a").hash() == new C(1, "a").hash());
    assert!(new C(1, "a").hash() != new C(2, "a").hash());
}

test("generic class and record enum") {
    let w = new Wrap(3);
    assert!(w.show() == "Wrap { inner: 3 }", w.show());
    assert!(w == new Wrap(3));
    let r = Sh::Rect { w: 1, h: 2 };
    assert!(r.show() == "Sh::Rect { w: 1, h: 2 }", r.show());
    assert!(r == Sh::Rect { w: 1, h: 2 });
    assert!(Sh::Dot < r);
    assert!(r.hash() == Sh::Rect { w: 1, h: 2 }.hash());
}

test("same-module trait impl reads a private field") {
    assert!(new C(7, "x").peek() == 7);
    let p = new Powers(1);
    let sum = 0;
    for v in p {
        sum += v;
    }
    assert!(sum == 2 + 4 + 8 + 16, sum.show());
}
