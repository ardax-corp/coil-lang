// Expected: E0116 — ?. on Result.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    let r = Result::Ok({ v: 1 });
    write_all(stdout(), to_bytes(format("%v", r?.v)));
}
