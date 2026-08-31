// Binary higher-kinded trait: Bifunctor<F: * -> * -> *>.
// Expected output: 42

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Bifunctor<F: * -> * -> *> {
    fn tag<A, B>(F<A, B> xs) -> int;
}

impl Bifunctor<Result> {
    pub fn tag<A, B>(Result<A, B> xs) -> int {
        return 42;
    }
}

fn get_tag<F: * -> * -> *, Bifunctor, A, B>(F<A, B> xs) -> int {
    return tag(xs);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", get_tag(Result::Ok(7)))));
}
