// Output: 4! 4 4 x! x x 5! 5 5 6! 6 6 7! 7 7 8! 8 8 1|2! 3|4!
// A trait instance method's own bounded type parameter (`fn third<W:
// Show>`) is passed on to a bounded generic (`shout(w)`) and shown
// (`show(w)`, `w.show()`): an `int` instance, a class instance and a
// bounded instance (`impl Pass for Box<T: Show>`), called directly and
// through a generic over the trait, with the bounded generic declared
// before and after the instances (#1471).
use io::{stdout, write};
use string::to_bytes;

trait Pass<S> {
    fn third<W: Show>(S s, W w) -> string {}
}

class Pt {
    pub x: int,
}

class Box<T> {
    pub v: T,
}

fn shout_before<X: Show>(X x) -> string {
    return show(x) + "!";
}

impl Pass for int {
    pub fn third<W: Show>(int s, W w) -> string {
        return shout_before(w) + " " + show(w) + " " + w.show();
    }
}

impl Pass for Pt {
    pub fn third<W: Show>(Pt s, W w) -> string {
        return shout_after(w) + " " + show(w) + " " + w.show();
    }
}

impl Pass for Box<T: Show> {
    pub fn third<W: Show>(Box<T> s, W w) -> string {
        return show(s.v) + "|" + shout_after(w);
    }
}

fn shout_after<X: Show>(X x) -> string {
    return show(x) + "!";
}

fn use_pass<P: Pass>(P p) -> string {
    return p.third(7) + " " + p.third(8);
}

fn main() {
    let text = 3.third(4);
    text = text + " " + 3.third("x");
    text = text + " " + new Pt(1).third(5) + " " + new Pt(1).third(6);
    text = text + " " + use_pass(3);
    text = text + " " + new Box(1).third(2) + " " + new Box(3).third(4);
    write(stdout(), to_bytes(text));
}
