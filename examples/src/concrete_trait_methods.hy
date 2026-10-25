// Trait methods on concrete receivers for tests/effects: an impl for
// `string` and one for `Vec<T>`, called from another module.

trait Shout<T> {
    fn shout(T s) -> string {}
}

impl Shout for string {
    pub fn shout(string s) -> string {
        return s + "!";
    }
}

trait Doubled<V> {
    type Item;
    fn each<U>(V xs, Item -> U f) -> Vec<U> {}
}

impl Doubled for Vec<T> {
    type Item = T;

    pub fn each<U>(Vec<T> xs, T -> U f) -> Vec<U> {
        let out: Vec<U> = Vec::new();
        let i = 0;
        while i < xs.len() {
            out.push(f(xs[i]));
            i = i + 1;
        }
        return out;
    }
}

static let SHOUTS: int = 0;

trait Loud<T> {
    fn loud(T s) -> string {}
}

impl Loud for string {
    pub fn loud(string s) -> string {
        SHOUTS = SHOUTS + 1;
        return s + "!!";
    }
}
