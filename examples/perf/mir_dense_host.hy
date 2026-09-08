// Hit bench for COI-290 W4: allowlisted HostInvoke inside a dense body.
// Parent (W3) refuses HostInvoke and stays fuse-IL. `hot` loops sin + mul/add.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float a, float dx, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = 0.125;
    while i < n {
        s = s + sin(x) * a + dx;
        x = x + dx;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(1.5, 0.000002, 400000) as int)));
}
