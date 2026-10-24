// A class whose function-typed field takes or returns the class, directly
// or through a container or a second class (#1989).
use tests::assert;

class Node {
    pub f: fn(Node) -> int,
}

class Lazy {
    pub kids: fn() -> Vec<Lazy>,
}

class Me {
    pub me: fn() -> Me,
    pub n: int,
}

class Ctx {
    pub cur: Option<Item>,
    pub timers: Vec<Timer>,
}

class Item {
    pub f: fn(Ctx) -> int,
}

class Timer {
    pub f: fn(Ctx) -> int,
}

test("a function field taking its own class") {
    let n = new Node(fn (Node x) => 7);
    let f = n.f;
    assert!(f(n) == 7);
}

test("a function field returning a Vec of its class") {
    let l = new Lazy(fn () => Vec::new());
    let f = l.kids;
    let v = f();
    assert!(v.len() == 0);
}

test("a function field returning its class") {
    let m = new Me(fn () => new Me(fn () => panic "x", 2), 1);
    let f = m.me;
    assert!(f().n == 2);
}

test("function fields through a second class") {
    let c = new Ctx(Option::None, Vec::new());
    c.timers.push(new Timer(fn (Ctx x) => x.timers.len()));
    let t = c.timers[0];
    let f = t.f;
    assert!(f(c) == 1);
}
