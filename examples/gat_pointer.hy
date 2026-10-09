// Generic associated types (GATs): `Ref<T>` is an associated type
// constructor. The generic `get` returns the projection `P::Ref<A>`, which
// the `Pointer<Option>` instance resolves to `A`.
//
// Output: 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

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

fn main() {
    write_all(stdout(), to_bytes(format("%i", get(Option::Some(42), 0))));
}
