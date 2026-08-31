// Unary higher-kinded trait: Container<F: * -> *>.
// Expected output: 42

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Container<F: * -> *> {
    fn first<A>(F<A> xs) -> A;
}

impl Container<Option> {
    pub fn first<A>(Option<A> xs) -> A {
        return match xs {
            Option::Some(v) => v,
            Option::None => 0,
        };
    }
}

fn get<F: Container, A>(F<A> xs) -> A {
    return first(xs);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", get(Option::Some(42)))));
}
