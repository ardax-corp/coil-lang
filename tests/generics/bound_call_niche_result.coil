// A bound trait call in a shared generic body whose result is a ground
// niche `Option` / `Result` (`Option<string>`, `Result<string, string>`):
// declared ground in the trait it is the instance's own word; typed with a
// type variable (`Option<A>` at `A = string`) the dictionary entry returns
// it boxed and it converts after the call (#1433).
use tests::assert;

trait Named<T> {
    fn name(T s, int b) -> Option<string> {}
    fn check(T s, Result<string, string> r) -> Result<string, string> {}
}

impl Named for int {
    fn name(int s, int b) -> Option<string> {
        if b > 0 {
            return Option::Some("n" + (s + b).show());
        }
        return Option::None;
    }

    fn check(int s, Result<string, string> r) -> Result<string, string> {
        return match r {
            Result::Ok(v) => Result::Ok(v + s.show()),
            Result::Err(e) => Result::Err(e + "!"),
        };
    }
}

trait Holder<F: * -> *> {
    fn peek<A>(F<A> xs) -> Option<A> {}
}

class Box1<T> {
    pub v: T,
    pub full: bool,
}

impl Holder for Box1 {
    pub fn peek<A>(Box1<A> b) -> Option<A> {
        if b.full {
            return Option::Some(b.v);
        }
        return Option::None;
    }
}

fn named<T: Named>(T s, int b) -> string {
    return match s.name(b) {
        Option::Some(v) => v,
        Option::None => "none",
    };
}

fn checked<T: Named>(T s, Result<string, string> r) -> string {
    return match s.check(r) {
        Result::Ok(v) => "ok " + v,
        Result::Err(e) => "err " + e,
    };
}

fn peeked<F: Holder>(F<string> xs) -> string {
    return match peek(xs) {
        Option::Some(v) => v,
        Option::None => "none",
    };
}

fn named_one<T: Named>(T s) -> string {
    return named(s, 1);
}

fn apply<A, B>(fn(A) -> B f, A a) -> B {
    return f(a);
}

test("ground niche results through a bound") {
    assert!(named(1, 2) == "n3");
    assert!(named(1, 0) == "none");
    assert!(checked(4, Result::Ok("a")) == "ok a4");
    assert!(checked(4, Result::Err("b")) == "err b!");
}

test("a niche result the trait types with a variable") {
    assert!(peeked(new Box1("x", true)) == "x");
    assert!(peeked(new Box1("y", false)) == "none");
}

test("the shared bodies, through function values") {
    let f = peeked;
    let a = new Box1("x", true);
    let b = new Box1("y", false);
    assert!(f(a) == "x");
    assert!(apply(peeked, b) == "none");
    assert!(apply(named_one, 3) == "n4");
}
