// Expected: compile failure — `T` has no `Describe` bound, so
// `Describe<Box<T>>` cannot get its context.
class Box<T> {
    pub item: T,
}

trait Describe<S> {
    fn describe(S x) -> string {}
}

impl Describe for Box<T: Describe> {
    pub fn describe(Box<T> b) -> string {
        return b.item.describe();
    }
}

fn wrap<T>(T x) -> string {
    return new Box(x).describe();
}

fn main() {}
