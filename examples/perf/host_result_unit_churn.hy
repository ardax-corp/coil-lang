// Host Result<(), IoError> pack: Ok = 0, Err = pointer (Option-shaped).
// HostInvoke-facing helper used like production `as_result_unit` natives.
// ITERS=20000000; i % 10 == 9 is Err. Checksum 18000000.
use io::{stdout, result_unit_probe};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    let iters = 20000000;
    let acc = 0;
    let i = 0;
    while i < iters {
        let n = i % 10;
        let code = if n == 9 { -1 } else { n };
        acc = acc + match result_unit_probe(code) {
            Result::Ok(_) => 1,
            Result::Err(_) => 0,
        };
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
