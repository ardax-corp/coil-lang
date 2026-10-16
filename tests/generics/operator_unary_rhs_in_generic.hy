// Output: 4 -4 -4 d7 true7
// A unary operator as the right operand of a binary one in a generic body
// whose operators go through the op traits (`x + -y`), and a shared generic
// body whose ground operands sit under a dictionary call.
use io::{stdout, write};
use string::{format, to_bytes};

class M {
    pub c: int,
}

impl Add for M {
    pub fn add(M a, M b) -> M {
        return new M(a.c + b.c);
    }
}

impl Neg for M {
    pub fn neg(M a) -> M {
        return new M(0 - a.c);
    }
}

fn f<T: Add + Neg>(T x, T y) -> T {
    return x + -y;
}

fn g<T: Add + Neg>(T x, T y) -> T {
    return -x + -(-y);
}

fn spread<A: Num + Show, B: Show>(A lo, A hi, B tag) -> string {
    return show(tag) + show(hi - lo);
}

fn main() {
    let r = f(new M(7), new M(3));
    let n = f(-1, 3);
    let q = g(new M(5), new M(1));
    let s = spread(3, 10, "d");
    let t = spread(3, 10, true);
    write(stdout(), to_bytes(format("%i %i %i %s %s", r.c, n, q.c, s, t)));
}
