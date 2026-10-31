// A trait default method reached through a bound (`x.two()` in
// `fn two_of<T: Dbl>`) whose instance is a generic one over `Option`
// (`impl Dbl for Option<T>`), at a niche `Option` (`Option<string>`): the
// mono clone calls the default body, which takes the enum object
// (coil-lang#1609).
use tests::assert;

trait Dbl<T> {
    fn one(T x) -> int {}

    fn two(T x) -> int {
        return x.one() * 2;
    }
}

impl Dbl for Option<T> {
    fn one(Option<T> o) -> int {
        return match o {
            Some(_) => 1,
            None => 0,
        };
    }
}

impl Dbl for int {
    fn one(int x) -> int {
        return x;
    }
}

fn two_of<T: Dbl>(T x) -> int {
    return x.two();
}

fn g<E>(E e) -> int {
    return two_of(Some(e));
}

test("a default through a bound at a niche Option") {
    assert!(two_of(Some("a")) == 2);
    let n: Option<string> = None;
    assert!(two_of(n) == 0);
}

test("a default through a bound at a boxed Option and an int") {
    assert!(two_of(Some(3)) == 2);
    assert!(two_of(4) == 8);
}

test("a default through a bound from a generic caller") {
    assert!(g("a") == 2);
    assert!(g(1) == 2);
}

test("a default through a bound as a PolyFn and as a method") {
    let f = two_of;
    assert!(f(Some("a")) == 2);
    let o: Option<string> = None;
    assert!(o.two() == 0);
}
