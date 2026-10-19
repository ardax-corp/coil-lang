// A mono clone of a bounded generic calls a generic instance's method
// (`impl Sz for Option<T>`) with a niche `Option` (`Option<string>`, an
// `Option` of a class): the instance's clone at that type takes the niche
// word, and past the clone cap its shared entry, which declares it open,
// takes its enum object (#1579).
use tests::assert;

trait Sz<T> {
    fn sz(T x) -> int {}
}

impl Sz for Option<T> {
    fn sz(Option<T> o) -> int {
        return match o {
            Some(_) => 1,
            None => 0,
        };
    }
}

trait Name<T> {
    fn name(T x) -> string {}
}

impl Name for string {
    fn name(string x) -> string {
        return x;
    }
}

impl Name for Option<T: Name> {
    fn name(Option<T> o) -> string {
        return match o {
            Some(v) => "some " + v.name(),
            None => "none",
        };
    }
}

class Cell {
    pub v: int,
}

class C0 {
    pub v: int,
}

class C1 {
    pub v: int,
}

class C2 {
    pub v: int,
}

class C3 {
    pub v: int,
}

class C4 {
    pub v: int,
}

class C5 {
    pub v: int,
}

class C6 {
    pub v: int,
}

class C7 {
    pub v: int,
}

class C8 {
    pub v: int,
}

fn size<T: Sz>(T x) -> int {
    return x.sz();
}

fn size2<T: Sz>(T x) -> int {
    return x.sz();
}

fn named<T: Name>(T x) -> string {
    return x.name();
}

fn h<E>(E e) -> int {
    let n: Option<E> = None;
    return size(n) + size(Some(e));
}

test("a ground call passes a niche Option to an open instance") {
    assert!(size(Some("b")) == 1);
    let n: Option<string> = None;
    assert!(size(n) == 0);
    assert!(size(Some(new Cell(1))) == 1);
}

test("a clone passes its niche Option to an open instance") {
    assert!(h("s") == 1);
    assert!(h(new Cell(2)) == 1);
    assert!(h(3) == 1);
}

test("an open instance with a context reads the niche Option's payload") {
    assert!(named(Some("x")) == "some x");
    let n: Option<string> = None;
    assert!(named(n) == "none");
}

test("past the instance's clone cap, its shared entry") {
    assert!(size2(Some(new C0(0))) == 1);
    let n0: Option<C0> = None;
    assert!(size2(n0) == 0);
    assert!(size2(Some(new C1(1))) == 1);
    let n1: Option<C1> = None;
    assert!(size2(n1) == 0);
    assert!(size2(Some(new C2(2))) == 1);
    let n2: Option<C2> = None;
    assert!(size2(n2) == 0);
    assert!(size2(Some(new C3(3))) == 1);
    let n3: Option<C3> = None;
    assert!(size2(n3) == 0);
    assert!(size2(Some(new C4(4))) == 1);
    let n4: Option<C4> = None;
    assert!(size2(n4) == 0);
    assert!(size2(Some(new C5(5))) == 1);
    let n5: Option<C5> = None;
    assert!(size2(n5) == 0);
    assert!(size2(Some(new C6(6))) == 1);
    let n6: Option<C6> = None;
    assert!(size2(n6) == 0);
    assert!(size2(Some(new C7(7))) == 1);
    let n7: Option<C7> = None;
    assert!(size2(n7) == 0);
    assert!(size(Some(new C8(8))) == 1);
    let n8: Option<C8> = None;
    assert!(size(n8) == 0);
}
