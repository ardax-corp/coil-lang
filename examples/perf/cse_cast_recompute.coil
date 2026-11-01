// Hit bench for local EarlyCSE: force CastIntToFloat recomputes that
// ssa_gvn does not number (varying i, first cast stored).
use io::sync::print;
use string::format;

fn hot(int n) -> int {
    let acc = 0.0;
    let i = 0;
    while i < n {
        let xf = i as float;
        acc = acc + xf;
        acc = acc + (i as float);
        acc = acc + (i as float);
        acc = acc + (i as float);
        i = i + 1;
    }
    return acc as int;
}

fn main() {
    let _ = print(format("%i", hot(1000000)));
}
