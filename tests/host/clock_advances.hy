// HostInvoke + virtual `clock`: wall/mono advance across a short sleep.
// wall and mono must advance, got {output:?}
// Output: 11
use clock::{mono_nanos, sleep_ms, wall_nanos};
use io::{stdout, write};
use string::{format, to_bytes};

fn flag(int later, int earlier) -> int {
    if later > earlier {
        return 1;
    }
    return 0;
}

fn main() {
    let w0 = wall_nanos();
    let m0 = mono_nanos();
    sleep_ms(15);
    let w1 = wall_nanos();
    let m1 = mono_nanos();
    write(stdout(), to_bytes(format("%i%i", flag(w1, w0), flag(m1, m0))));
}
