// Output: 2 2.5 7 x! 4b! 9 1!
// A function-style trait call on a ground argument inside a shared
// generic body (reached through a function value, and in a trait
// instance's method) resolves to the argument's own instance, not to an
// unrelated bound active in that body (#1483).
use io::{stdout, write};
use string::to_bytes;

trait Pass<S> {
    fn third<W: Show>(S s, W w) -> string {}
}

class Box<T> {
    pub v: T,
}

impl Pass for int {
    pub fn third<W: Show>(int s, W w) -> string {
        return show(s) + show(w) + "!";
    }
}

impl Pass for Box<T: Show> {
    pub fn third<W: Show>(Box<T> s, W w) -> string {
        return third(4, w);
    }
}

fn f<W: Show>(W w, int x) -> string {
    return show(x) + " " + show(w);
}

fn g<A: Show, B: Show>(A a, B b, int n) -> string {
    return show(n) + " " + show(a) + show(b) + "!";
}

fn main() {
    let h = f;
    let k = g;
    let text = h(2.5, 2) + " " + k("x", "", 7) + " " + new Box(true).third("b");
    text = text + " " + show(9) + " " + third(1, "");
    let _ = write(stdout(), to_bytes(text));
}
