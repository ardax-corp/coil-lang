// A trait over a two-argument type constructor (`F: * -> * -> *`) has an
// instance for `Result`, and a generic caller reaches it through the bound.
use tests::assert;

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

test("Result instance of a binary constructor trait") {
    assert!(get_tag(Result::Ok(7)) == 42);
    let r: Result<int, string> = Result::Err("e");
    assert!(get_tag(r) == 42);
}
