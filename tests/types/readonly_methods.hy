// Methods, trait objects and generic calls on `readonly` values: whatever
// only reads is allowed (#1558).
use collections::map::HashMap;
use tests::assert;

trait Area<A> {
    fn area(readonly A self) -> int {}
}

class Sq {
    pub s: int,
}

impl Area for Sq {
    fn area(readonly Sq self) -> int {
        return self.s * self.s;
    }
}

impl Sq {
    pub fn side() -> int {
        return self.s;
    }

    pub fn grown() -> Sq {
        return new Sq(self.side() + 1);
    }
}

fn total<T: Area>(T x) -> int {
    return x.area();
}

fn first_side(Vec<readonly Sq> xs) -> int {
    return xs[0].side();
}

test("read-only methods on a readonly receiver") {
    let p = readonly new Sq(3);
    assert!(p.side() == 3);
    assert!(p.grown().side() == 4);
    assert!(p.area() == 9);
}

test("readonly trait objects and generic bounds") {
    let p = readonly new Sq(3);
    let a: readonly Area = p;
    assert!(a.area() == 9);
    assert!(total(p) == 9);
    assert!(total(new Sq(2)) == 4);
    assert!(first_side([p]) == 3);
}

test("stdlib methods that only read") {
    let m: HashMap<string, int> = HashMap::new();
    m.insert("a", 1);
    let r = readonly m;
    assert!(r.get("a", 0) == 1);
    assert!(r.contains("a"));
}

test("Option payloads stay readable") {
    let o: readonly Option<Sq> = Option::Some(new Sq(5));
    let n = o.map(fn (readonly Sq q) => q.side()).unwrap_or(0);
    assert!(n == 5);
}
