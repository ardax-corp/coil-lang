// @output 4! 4 4 x! x x 5! 5 5 6! 6 6 7! 7 7 8! 8 8
// A trait default body's own bounded type parameter (`fn third<W:
// Show>`) is passed on to a bounded generic (`shout(w)`) and shown
// (`show(w)`, `w.show()`), for an `int` instance and a class instance,
// called directly and through a generic over the trait (#1471).
use io::{stdout, write};
use string::to_bytes;

trait Pass<S> {
    fn third<W: Show>(S s, W w) -> string {
        return shout(w) + " " + show(w) + " " + w.show();
    }
}

fn shout<X: Show>(X x) -> string {
    return show(x) + "!";
}

class Pt {
    pub x: int,
}

impl Pass for int {
}

impl Pass for Pt {
}

fn use_pass<P: Pass>(P p) -> string {
    return p.third(7) + " " + p.third(8);
}

fn main() {
    let text = 3.third(4);
    text = text + " " + 3.third("x");
    text = text + " " + new Pt(1).third(5) + " " + new Pt(1).third(6);
    text = text + " " + use_pass(new Pt(2));
    let _ = write(stdout(), to_bytes(text));
}
