// Unary higher-kinded trait: Container<F: * -> *>.
//
// Output: 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

trait Container<F: * -> *> {
    fn first<A>(F<A> xs, A fallback) -> A {}
}

impl Container for Option {
    pub fn first<A>(Option<A> xs, A fallback) -> A {
        return match xs {
            Option::Some(v) => v,
            Option::None => fallback,
        };
    }
}

fn get<F: Container, A>(F<A> xs, A fallback) -> A {
    return first(xs, fallback);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", get(Option::Some(42), 0))));
}
