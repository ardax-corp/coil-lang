// @expected E0119 — the trait declares `type Ref<T>`; the impl's `type Ref` drops
// the type parameter.
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T> {}
}

impl Pointer for Option {
    type Ref = int;
    fn deref<T>(Option<T> ptr) -> T {
        return 0;
    }
}

fn main() {}
