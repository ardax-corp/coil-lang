// @output 3 0 2 1 9 9
// A trait default body with its own bounded type parameter (`fn count<P:
// Probe>`) gets a per-type clone at each (receiver, own type) pair, so
// the bound's method is called directly; two own types at one receiver
// and one own type at two receivers each keep their own behaviour.
use io::{stdout, write};
use string::to_bytes;

trait Probe<T> {
    fn hit(T p, int i) -> bool {}
}

class Even {
    pub n: int,
}

class Never {
    pub n: int,
}

impl Probe for Even {
    fn hit(Even p, int i) -> bool {
        return i % 2 == 0;
    }
}

impl Probe for Never {
    fn hit(Never p, int i) -> bool {
        return false;
    }
}

trait Counted<S> {
    fn size(S s) -> int {}

    fn count<P: Probe>(S s, P p) -> int {
        let n = 0;
        let i = 0;
        while i < s.size() {
            if p.hit(i) {
                n = n + 1;
            }
            i = i + 1;
        }
        return n;
    }
}

impl Counted for int {
    fn size(int s) -> int {
        return s;
    }
}

impl Counted for string {
    fn size(string s) -> int {
        return s.len();
    }
}

fn main() {
    let e = new Even(0);
    let z = new Never(0);
    let a = 5.count(e);
    let b = 5.count(z);
    let c = "abc".count(e);
    let d = "x".count(e);
    let f = 17.count(e);
    let g = "abcdefghijklmnopq".count(e);
    let text = show(a) + " " + show(b) + " " + show(c) + " " + show(d) + " " + show(f) + " " +
               show(g);
    let _ = write(stdout(), to_bytes(text));
}
