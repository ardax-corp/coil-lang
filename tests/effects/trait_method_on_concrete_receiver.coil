// A trait method called on a receiver whose type names the impl costs what
// the impl's body does, as an inherent method does: a pure one passes
// `uses {}` and runs in `comptime`, one that writes a static still says so
// (#2008).
use tests::assert;
use concrete_trait_methods::{Shout, Doubled, Loud};

trait Trimmed<T> {
    fn trimmed(T s) -> string {}
}

impl Trimmed for string {
    pub fn trimmed(string s) -> string {
        return s + "";
    }
}

fn local_impl(string s) -> string uses {} {
    return s.trimmed();
}

fn other_module_impl(string s) -> string uses {} {
    return s.shout();
}

fn generic_impl(Vec<int> xs) -> Vec<int> uses {} {
    return xs.each(fn (int x) => x * 2);
}

test("pure trait methods on concrete receivers are pure") {
    assert!(local_impl("a") == "a");
    assert!(other_module_impl("a") == "a!");
    assert!(generic_impl([1, 2])[1] == 4);
}

test("compile-time code calls them") {
    let s = comptime "hi".shout();
    let n = comptime {
        let xs: Vec<int> = [1, 2, 3];
        return xs.each(fn (int x) => x + 1).len();
    };
    assert!(s == "hi!" && n == 3);
}

test("an impl that writes a static is not pure") {
    assert!("a".loud() == "a!!");
}
