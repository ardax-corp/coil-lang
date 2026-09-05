// Hit bench for dest_prop: in-loop slot alias of a class, then several
// GetField uses. copy_prop refuses GetField-shaped loads; slot_promote
// clears aliases at the first GetField, so later field reads stay on the
// dest slot unless dest_prop forwards them.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Cell {
    pub a: int,
    pub b: int,
    pub c: int,
    pub d: int,
}

fn hot(Cell n, int iters) -> int {
    let s = 0;
    let i = 0;
    while i < iters {
        let p = n;
        s = s + p.a;
        s = s + p.b;
        s = s + p.c;
        s = s + p.d;
        s = s + p.a;
        s = s + p.b;
        s = s + p.c;
        s = s + p.d;
        i = i + 1;
    }
    return s;
}

fn main() {
    let n = new Cell(1, 2, 3, 4);
    write_all(stdout(), to_bytes(format("%i", hot(n, 2000000))));
}
