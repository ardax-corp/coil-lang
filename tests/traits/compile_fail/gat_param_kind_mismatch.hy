// Expected: E0119 — `Ref<F: * -> *>` takes a type constructor; `Ref<T>` passes a
// plain type `T`.
trait Pointer<P: * -> *> {
    type Ref<F: * -> *>;
    fn bad<T>(P<T> ptr) -> Ref<T> {}
}

fn main() {}
