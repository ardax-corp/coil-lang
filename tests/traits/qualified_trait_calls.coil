// `Trait::method(args)` is the free call `method(args)` with only that
// trait's method as a candidate: it picks between traits (or a free fn)
// sharing a method name, at a ground type, under a bound, in a default
// body, on a trait object, for receiverless methods and for a trait
// declared in another module.
use tests::assert;
use trait_import_codec::{Node, DecodeError, FromNode};

trait Area<T> {
    fn size(T s) -> int {}

    fn doubled(T s) -> int {
        return Area::size(s) * 2;
    }
}

trait Volume<T> {
    fn size(T s) -> int {}
}

trait Make<S> {
    static fn make(int n) -> S {}
}

trait Zero<S> {
    fn zero() -> S {}
}

class Sq {
    pub w: int,
}

impl Area for Sq {
    pub fn size(Sq s) -> int {
        return s.w * s.w;
    }
}

impl Volume for Sq {
    pub fn size(Sq s) -> int {
        return s.w * s.w * s.w;
    }
}

impl Area for int {
    pub fn size(int n) -> int {
        return n + 100;
    }
}

impl Make for Sq {
    pub static fn make(int n) -> Sq {
        return new Sq(n);
    }
}

impl Zero for Sq {
    pub fn zero() -> Sq {
        return new Sq(0);
    }
}

impl FromNode for Sq {
    pub fn from_node(Sq proto, Node n) -> Result<Sq, DecodeError> {
        return Result::Ok(new Sq(n.i));
    }
}

// A free fn with the trait methods' name: the bare `size(x)` is this one.
fn size(int x) -> int {
    return -x;
}

fn area_of<T: Area>(T x) -> int {
    return Area::size(x);
}

fn both<T: Area + Volume>(T x) -> int {
    return Area::size(x) * 1000 + Volume::size(x);
}

fn fresh<T: Zero>() -> T {
    return Zero::zero();
}

fn made<T: Make>() -> T {
    return Make::make(7);
}

fn object_size(Area a) -> int {
    return Area::size(a);
}

test("concrete type") {
    let s = new Sq(3);
    assert!(Area::size(s) == 9);
    assert!(Volume::size(s) == 27);
    assert!(Area::size(5) == 105);
}

test("a free fn of the same name stays the bare call") {
    assert!(size(4) == -4);
    assert!(Area::size(4) == 104);
}

test("generic body") {
    let s = new Sq(2);
    assert!(area_of(s) == 4);
    assert!(area_of(1) == 101);
    assert!(both(s) == 4008);
}

test("default method body") {
    assert!(Area::doubled(new Sq(3)) == 18);
    assert!(new Sq(3).doubled() == 18);
}

test("trait object") {
    assert!(object_size(new Sq(4)) == 16);
    assert!(object_size(6) == 106);
}

test("receiverless methods") {
    let a: Sq = Make::make(5);
    let b: Sq = Zero::zero();
    let c: Sq = fresh();
    let d: Sq = made();
    assert!(a.w == 5);
    assert!(b.w == 0);
    assert!(c.w == 0);
    assert!(d.w == 7);
}

test("trait from another module") {
    let s = new Sq(1);
    let r = FromNode::from_node(s, new Node(11, ""));
    let w = match r {
        Result::Ok(q) => q.w,
        Result::Err(_) => -1,
    };
    assert!(w == 11);
    assert!(trait_import_codec::FromNode::port_or(s, new Node(12, ""), 0) == 12);
}
