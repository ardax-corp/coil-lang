// `symbol::fn_key()` in generic code is a key per instantiation: the same
// for every call at the same types, and different at other types, through
// wrappers, lambdas, generic class methods and past the clone budget.
// @flags --clone-budget 8
use tests::assert;
use symbol::fn_key;

fn ident<T>(T x) -> int {
    return fn_key();
}

fn pair<A, B>(A a, B b) -> int {
    return fn_key();
}

fn wrap<T>(T x) -> int {
    return ident(x);
}

fn lam<T>(T x) -> int {
    let f = fn () => fn_key();
    return f();
}

class Holder<T> {
    pub v: T,
}

impl Holder<T> {
    pub fn key() -> int {
        return fn_key();
    }
}

class C1 {
    pub a: int,
}

class C2 {
    pub a: int,
}

class C3 {
    pub a: int,
}

class C4 {
    pub a: int,
}

class C5 {
    pub a: int,
}

test("a key per instantiation") {
    assert!(ident(1) == ident(2));
    assert!(ident("a") == ident("b"));
    assert!(ident(1) != ident("a"));
    assert!(ident(1) != ident(1.5));
    assert!(pair(1, "a") != pair("a", 1));
    assert!(pair(1, "a") == pair(2, "b"));
}

test("through a generic wrapper") {
    assert!(wrap(1) == ident(1));
    assert!(wrap("a") == ident("a"));
    assert!(wrap(1) != wrap("a"));
}

test("a lambda in a generic body") {
    assert!(lam(1) == lam(2));
    assert!(lam(1) != lam("a"));
}

test("a generic class's method") {
    let a = new Holder(1);
    let b = new Holder("x");
    assert!(a.key() == new Holder(5).key());
    assert!(a.key() != b.key());
}

test("more instantiations than the clone budget") {
    let keys = [
        ident(1),
        ident("a"),
        ident(1.5),
        ident(true),
        ident(new C1(1)),
        ident(new C2(1)),
        ident(new C3(1)),
        ident(new C4(1)),
        ident(new C5(1)),
        ident([1, 2]),
    ];
    let i = 0;
    while i < keys.len() {
        let j = i + 1;
        while j < keys.len() {
            assert!(keys[i] != keys[j]);
            j += 1;
        }
        i += 1;
    }
    assert!(ident(new C5(2)) == keys[8]);
}
