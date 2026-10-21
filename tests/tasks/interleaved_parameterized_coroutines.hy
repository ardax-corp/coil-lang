// Two handles from the same parameterized `gen fn`, resumed alternately and
// inline as call arguments, each keep their own argument and position.
// Output: 1,100,2,101,3,102
use io::{stdout, write};
use string::{format, to_bytes};

gen fn counter(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}

fn main() {
    let a = counter(1);
    let b = counter(100);

    let _ = write(stdout(), to_bytes(format("%i,", resume a)));
    let _ = write(stdout(), to_bytes(format("%i,", resume b)));
    let _ = write(stdout(), to_bytes(format("%i,", resume a)));
    let _ = write(stdout(), to_bytes(format("%i,", resume b)));
    let _ = write(stdout(), to_bytes(format("%i,", resume a)));
    let _ = write(stdout(), to_bytes(format("%i", resume b)));
}
