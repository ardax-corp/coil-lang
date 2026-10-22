// @output 2.5! 2.5! 2.5! z! dz! 7! d7! btrue! dbtrue! 1.5! q! d5! 3a! 24.5!
// A trait method's own bound (`fn third<W: Show>`) open at the call: a
// generic body forwards its own dictionary to a direct, a function-style
// and a bound call, a default body calls its sibling with it, and a
// dictionary's adapter (an `Option` over the trait's parameter) passes it
// on (#1471).
use io::{stdout, write};
use string::to_bytes;

trait Pass<S> {
    fn third<W: Show>(S s, W w) -> string {}
    fn deft<W: Show>(S s, W w) -> string {
        return "d" + s.third(w);
    }
}

trait Pick<S> {
    fn pick<W: Show>(S s, Option<S> o, W w) -> string {}
}

fn shout<X: Show>(X x) -> string {
    return show(x) + "!";
}

fn num(int x) -> string {
    return show(x);
}

class Pt {
    pub x: int,
}

class Box<T> {
    pub v: T,
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

impl Pass for Box<T: Show> {
    pub fn third<W: Show>(Box<T> s, W w) -> string {
        return show(s.v) + shout(w);
    }
}

impl Pick for int {
    pub fn pick<W: Show>(int s, Option<int> o, W w) -> string {
        let n = match o {
            Option::Some(v) => v,
            Option::None => 0,
        };
        return num(n + s) + shout(w);
    }
}

fn forward<W: Show>(W w) -> string {
    return 3.third(w) + " " + third(3, w) + " " + new Pt(2).third(w);
}

fn both<P: Pass, W: Show>(P p, W w) -> string {
    return p.third(w) + " " + p.deft(w);
}

fn picked<P: Pick, W: Show>(P p, Option<P> o, W w) -> string {
    return p.pick(o, w);
}

fn main() {
    let text = forward(2.5);
    text = text + " " + both(3, "z") + " " + both(new Pt(4), 7) + " " + both(new Box("b"), true);
    text = text + " " + third(1, 1.5) + " " + third(new Pt(9), "q") + " " + 3.deft(5);
    let f = picked;
    text = text + " " + f(1, Option::Some(2), "a") + " " + f(2, Option::None, 4.5);
    let _ = write(stdout(), to_bytes(text));
}
