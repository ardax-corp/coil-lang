// Unary higher-kinded trait: Container<F: * -> *>.
//
// @output 42

use io::sync::print;
use string::format;

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
    let _ = print(format("%i", get(Option::Some(42), 0)));
}
