// Binary higher-kinded trait: Bifunctor<F: * -> * -> *>.
//
// @output 42

use io::sync::print;
use string::format;

trait Bifunctor<F: * -> * -> *> {
    fn tag<A, B>(F<A, B> xs) -> int {}
}

impl Bifunctor for Result {
    pub fn tag<A, B>(Result<A, B> xs) -> int {
        return 42;
    }
}

fn get_tag<F: * -> * -> *, Bifunctor, A, B>(F<A, B> xs) -> int {
    return tag(xs);
}

fn main() {
    let _ = print(format("%i", get_tag(Result::Ok(7))));
}
