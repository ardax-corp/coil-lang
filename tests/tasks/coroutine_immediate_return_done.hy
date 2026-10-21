// Immediate-return gen fn completes on first resume; `done` flips after.
// Output: false,true
use io::{stdout, write};
use string::{format, to_bytes};

gen fn unit_ret() {
    return;
}

fn main() {
    let h = unit_ret();
    let _ = write(stdout(), to_bytes(format("%z,", done(h))));
    let _ = resume h;
    let _ = write(stdout(), to_bytes(format("%z", done(h))));
}
