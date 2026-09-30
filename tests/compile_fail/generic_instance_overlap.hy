// Expected: compile failure — `Box<T>` covers `Box<int>`.
class Box<T> {
    pub item: T,
}

trait Describe<S> {
    fn describe(S x) -> string {}
}

impl Describe for Box<T> {
    pub fn describe(Box<T> b) -> string {
        return "Box";
    }
}

impl Describe for Box<int> {
    pub fn describe(Box<int> b) -> string {
        return "Box<int>";
    }
}

fn main() {}
