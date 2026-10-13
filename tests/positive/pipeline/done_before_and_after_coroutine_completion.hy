// `done(h)` is false before the first resume and true after completion
// without needing another resume (M6: extra resume panics).
// Output: false,1,false,2,true
use io::{stdout, write};
use string::{format, to_bytes};

gen fn counter() {
    yield 1;
    return 2;
}

fn main() {
    let h = counter();
    write(stdout(), to_bytes(format("%z,", done(h))));
    write(stdout(), to_bytes(format("%i,", resume h)));
    write(stdout(), to_bytes(format("%z,", done(h))));
    write(stdout(), to_bytes(format("%i,", resume h)));
    write(stdout(), to_bytes(format("%z", done(h))));
}
