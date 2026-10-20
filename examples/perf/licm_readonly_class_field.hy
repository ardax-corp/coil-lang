// Hit bench for #1560 MIR LICM: readonly class field reads in a loop that
// also writes a field of another object. A field store could change a
// field of an ordinary class, but never one of a `readonly class`, so
// `c.scale` and `c.bias` move to the preheader.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

readonly class Cfg {
    pub scale: int,
    pub bias: int,
}

class Stats {
    pub sum: int,
}

fn apply(Cfg c, Vec<int> xs, Stats st) {
    let n = xs.len();
    for i in 0..n {
        st.sum = st.sum + xs[i] * c.scale + c.bias;
    }
}

fn main() {
    let n = 100000;
    let xs: Vec<int> = Vec::with_capacity(n);
    for i in 0..n {
        xs.push(i % 1000);
    }
    let st = new Stats(0);
    let s = 0;
    for r in 0..100 {
        apply(new Cfg(3, r), xs, st);
        s = s + st.sum % 997;
    }
    write_all(stdout(), to_bytes(format("%i", s)));
}
