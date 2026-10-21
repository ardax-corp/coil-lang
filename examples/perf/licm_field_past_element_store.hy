// Hit bench for #1559 MIR LICM: class field reads in a loop that stores
// array elements. An element store cannot change a field, so `c.scale` and
// `c.bias` move to the preheader.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

class Cfg {
    pub scale: int,
    pub bias: int,
}

fn apply(Cfg c, Vec<int> xs, Vec<int> out) {
    let n = xs.len();
    for i in 0..n {
        out[i] = xs[i] * c.scale + c.bias;
    }
}

fn main() {
    let n = 100000;
    let xs: Vec<int> = Vec::with_capacity(n);
    let out: Vec<int> = Vec::with_capacity(n);
    for i in 0..n {
        xs.push(i % 1000);
        out.push(0);
    }
    let c = new Cfg(3, 1);
    let s = 0;
    for r in 0..100 {
        c.bias = r;
        apply(c, xs, out);
        s = s + out[r];
    }
    let _ = write_all(stdout(), to_bytes(format("%i", s)));
}
