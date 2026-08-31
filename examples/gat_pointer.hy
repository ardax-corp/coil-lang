// Expected output: 42
//
// Phase 3 advanced generics: generic associated types (GATs).
// `Ref<T>` is an associated type constructor. The generic `get`
// returns the applied projection `P::Ref<A>`, pinned by the
// `Pointer<Option>` instance to `A`.

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T>;
}

impl Pointer for Option {
    type Ref<T> = T;
    pub fn deref<T>(Option<T> ptr) -> T {
        return match ptr {
            Option::Some(v) => v,
            Option::None => 0,
        };
    }
}

fn get<P: * -> *, Pointer, A>(P<A> ptr) -> P::Ref<A> {
    return deref(ptr);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", get(Option::Some(42)))));
}
