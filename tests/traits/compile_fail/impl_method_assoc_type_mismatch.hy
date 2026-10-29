// @expected E0102 — the impl's `next` returns Option<string>, the trait declares Option<Item> with `type Item = int` (#2086).
use io::sync::println;
use string::format;

class P {
    pub n: int,
}

impl Iterator for P {
    type Item = int;
    fn next(P p) -> Option<string> {
        if p.n > 0 {
            p.n -= 1;
            return Option::Some("x");
        }
        return Option::None;
    }
}

impl IntoIterator for P {
    type Item = int;
    type IntoIter = P;
    fn into_iter(P p) -> P {
        return p;
    }
}

fn main() {
    for x in new P(2) {
        let y: int = x + 1;
        let _ = println(format("%i", y));
    }
}
