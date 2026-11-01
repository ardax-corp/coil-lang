// A trait over a type constructor (`F: * -> *`) has instances for `Option`
// and a user generic class; generic code over `F<A>` works at each, for
// several element types. A generic associated type (`type Ref<T>`) is
// resolved by the instance.
use tests::assert;

trait Container<F: * -> *> {
    fn first<A>(F<A> xs, A fallback) -> A {}
    fn size<A>(F<A> xs) -> int {}
}

class Pair<T> {
    pub a: T,
    pub b: T,
}

impl Container for Option {
    pub fn first<A>(Option<A> xs, A fallback) -> A {
        return match xs {
            Option::Some(v) => v,
            Option::None => fallback,
        };
    }

    pub fn size<A>(Option<A> xs) -> int {
        return match xs {
            Option::Some(_) => 1,
            Option::None => 0,
        };
    }
}

impl Container for Pair {
    pub fn first<A>(Pair<A> p, A fallback) -> A {
        return p.a;
    }

    pub fn size<A>(Pair<A> p) -> int {
        return 2;
    }
}

fn get<F: Container, A>(F<A> xs, A fallback) -> A {
    return first(xs, fallback);
}

fn count<F: Container, A>(F<A> xs) -> int {
    return size(xs);
}

trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr, T fallback) -> Ref<T> {}
}

impl Pointer for Option {
    type Ref<T> = T;

    pub fn deref<T>(Option<T> ptr, T fallback) -> T {
        return ptr ?? fallback;
    }
}

fn load<P: * -> *, Pointer, A>(P<A> ptr, A fallback) -> P::Ref<A> {
    return deref(ptr, fallback);
}

test("an Option instance at several element types") {
    assert!(get(Option::Some(42), 0) == 42);
    assert!(get(Option::None, 7) == 7);
    assert!(get(Option::Some("s"), "") == "s");
    assert!(count(Option::Some(1.5)) == 1);
}

test("a user generic class instance") {
    assert!(get(new Pair(3, 4), 0) == 3);
    assert!(get(new Pair("x", "y"), "") == "x");
    assert!(count(new Pair(true, false)) == 2);
}

test("a generic associated type") {
    assert!(load(Option::Some(5), 0) + 1 == 6);
    assert!(load(Option::None, 9) == 9);
}
