// Expected: E0119 — `Pointer::Ref` takes one type argument; `P::Ref<A, int>` passes two.
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T> {}
}

fn bad<P: * -> *, Pointer, A>(P<A> ptr) -> P::Ref<A, int> {
    return deref(ptr);
}

fn main() {}
