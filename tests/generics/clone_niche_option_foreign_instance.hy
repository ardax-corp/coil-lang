// Another module's generic instance over `Option` / `Result` (`impl Sz
// for Option<T>`) called at a niche type (`Option<string>`): its clone
// there calls the shared method, which takes and returns the open enum
// object, while its callers pass and take the niche word: from a ground
// call, a mono clone, a direct method call, through the instance's
// dictionary and through a `PolyFn` (#1605).
use tests::assert;
use niche_option_instance_provider::{Sz, Id, Nm, Pick, lsize, lkeep};

class Cell {
    pub v: int,
}

fn size<T: Sz>(T x) -> int {
    return x.sz();
}

fn keep<T: Id>(T x) -> T {
    return x.id();
}

fn named<T: Nm>(T x) -> string {
    return x.nm();
}

fn pk<T: Pick>(T a, T b) -> T {
    return a.pick(b);
}

fn h<E>(E e) -> int {
    let n: Option<E> = None;
    return size(n) + size(Some(e)) * 10 + lsize(Some(e)) * 100;
}

fn hk<E>(E e) -> Option<E> {
    return keep(Some(e));
}

fn hkn<E>(E e) -> Option<E> {
    let n: Option<E> = None;
    return keep(n);
}

fn hl<E>(E e) -> Option<E> {
    return lkeep(Some(e));
}

fn hp<E>(E e) -> Option<E> {
    let n: Option<E> = None;
    return pk(n, Some(e));
}

fn hd<E>(E e) -> Option<E> {
    let o: Option<E> = Some(e);
    return o.id();
}

fn hds<E>(E e) -> int {
    let o: Option<E> = Some(e);
    return o.sz();
}

fn hr<E>(E e) -> int {
    let r: Result<E, string> = Ok(e);
    let q: Result<E, string> = Err("x");
    return size(r) * 10 + size(q);
}

fn text(Option<string> o) -> string {
    return match o {
        Some(v) => v,
        None => "none",
    };
}

test("a ground call passes and takes the niche word") {
    assert!(size(Some("b")) == 1);
    let n: Option<string> = None;
    assert!(size(n) == 0);
    assert!(size(Some(3)) == 1);
    assert!(text(keep(Some("a"))) == "a");
    assert!(text(keep(n)) == "none");
    assert!(named(Some("x")) == "some x");
    assert!(named(n) == "none");
    assert!(text(pk(n, Some("q"))) == "q");
    let r: Result<string, string> = Ok("a");
    assert!(size(r) == 1);
}

test("a mono clone calls the instance through its dictionary") {
    assert!(h("s") == 110);
    assert!(h(new Cell(2)) == 110);
    assert!(h(4) == 110);
    assert!(text(hk("a")) == "a");
    assert!(text(hkn("a")) == "none");
    assert!(text(hl("z")) == "z");
    assert!(text(hp("p")) == "p");
    assert!(hr("r") == 15);
    assert!(hr(new Cell(3)) == 15);
}

test("a direct method call on a niche Option") {
    assert!(text(hd("d")) == "d");
    assert!(hds("d") == 1);
    let o: Option<string> = Some("c");
    assert!(text(o.id()) == "c");
    assert!(o.sz() == 1);
    let r: Result<string, string> = Err("e");
    assert!(r.sz() == 5);
}

test("a PolyFn call through the instance's clone") {
    let f = keep;
    assert!(text(f(Some("b"))) == "b");
    let g = pk;
    let n: Option<string> = None;
    assert!(text(g(n, Some("g"))) == "g");
}
