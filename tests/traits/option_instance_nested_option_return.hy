// A generic instance over `Option` (`impl Wrap for Option<T>`) of a trait
// method returning `Option<T>`: the instance returns `Option<Option<T>>`,
// which its dictionary adapter returns as the instance spells its head
// (coil-lang#1609).
use tests::assert;

trait Wrap<T> {
    fn wrap(T x) -> Option<T> {}
}

impl Wrap for Option<T> {
    fn wrap(Option<T> o) -> Option<Option<T>> {
        return match o {
            Some(_) => Some(o),
            None => None,
        };
    }
}

impl Wrap for int {
    fn wrap(int x) -> Option<int> {
        return Some(x + 1);
    }
}

fn wrapped<T: Wrap>(T x) -> Option<T> {
    return x.wrap();
}

fn depth(Option<Option<string>> o) -> int {
    return match o {
        Some(Some(_)) => 2,
        Some(None) => 1,
        None => 0,
    };
}

test("a method call returns a nested Option") {
    let o: Option<int> = Some(3);
    assert!(o.wrap() == Some(Some(3)));
    let n: Option<int> = None;
    assert!(n.wrap() == None);
    let s: Option<string> = Some("a");
    assert!(depth(s.wrap()) == 2);
}

test("a bound call returns a nested Option") {
    assert!(wrapped(Some(3)) == Some(Some(3)));
    assert!(depth(wrapped(Some("a"))) == 2);
    let n: Option<string> = None;
    assert!(depth(wrapped(n)) == 0);
    assert!(wrapped(4) == Some(5));
}

test("a PolyFn call returns a nested Option") {
    let f = wrapped;
    assert!(depth(f(Some("b"))) == 2);
    assert!(f(Some(2)) == Some(Some(2)));
}
