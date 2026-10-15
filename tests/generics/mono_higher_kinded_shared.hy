// A generic function over a higher-kinded parameter whose result is an
// associated-type projection (`P::Ref<A>`) is not cloned per call: its
// `P<A>` formal binds `P` to a type constructor, and the clone could not
// resolve the trait callee through the projection. It keeps the shared
// body, at an int and a string type argument alike. So does one under a
// multi-parameter constraint whose second parameter is only in the result.
use tests::assert;

trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr, T fallback) -> Ref<T> {}
}

impl Pointer for Option {
    type Ref<T> = T;
    pub fn deref<T>(Option<T> ptr, T fallback) -> T {
        return match ptr {
            Option::Some(v) => v,
            Option::None => fallback,
        };
    }
}

fn get<P: * -> *, Pointer, A>(P<A> ptr, A fallback) -> P::Ref<A> {
    return deref(ptr, fallback);
}

trait Convert<A, B> {
    fn cast(A x) -> B {}
}

impl Convert<int> for int {
    pub fn cast(int x) -> int {
        return x + 1;
    }
}

fn apply_cast<A, B>(A x) -> B where Convert<A, B> {
    return cast(x);
}

test("a higher-kinded P and a P::Ref<A> result") {
    assert!(get(Option::Some(42), 0) == 42);
    assert!(get(Option::None, 7) == 7);
    assert!(get(Option::Some("hk"), "none") == "hk");
    assert!(get(Option::None, 2.5) == 2.5);
}

test("a multi-parameter constraint, B only in the result") {
    let n: int = apply_cast(41);
    assert!(n == 42);
}
