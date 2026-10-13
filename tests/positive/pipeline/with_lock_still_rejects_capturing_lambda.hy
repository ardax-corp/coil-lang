// Capturing lambda is still refused by `with_lock` (runtime NotSendable).
// Output: 0
use thread::{mutex, with_lock};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let extra = 1;
    let m = mutex(0)?;
    let code = match with_lock(m, fn (int x) use (extra) => (x, extra)) {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    write_all(stdout(), to_bytes(format("%i", code)));
}
