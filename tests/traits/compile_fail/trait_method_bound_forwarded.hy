// Expected: E0801 — a trait instance method's own bounded type parameter
// (`fn third<W: Show>`) has no dictionary in the method's frame yet, so
// passing it to a bounded generic is refused at compile time; it used to
// pass no dictionary, or the instance's own, and fail at run time (#1471).
trait Pass<S> {
    fn third<W: Show>(S s, W w) -> string {}
}

class Pt {
    pub x: int,
}

fn shout<X: Show>(X x) -> string {
    return show(x) + "!";
}

impl Pass for int {
    pub fn third<W: Show>(int s, W w) -> string {
        return shout(w);
    }
}

impl Pass for Pt {
    pub fn third<W: Show>(Pt s, W w) -> string {
        return shout(w);
    }
}

fn main() {
    let _ = 3.third(4);
    let _ = new Pt(1).third(5);
}
