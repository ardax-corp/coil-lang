// Host Result<(), IoError> pack: Ok = 0, Err = pointer (Option-shaped).
// HostInvoke-facing helper used like production `as_result_unit` natives.
// ITERS=20000000; i % 10 == 9 is Err. Checksum 18000000.
use io::result_unit_probe;
use io::sync::print;
use string::format;

fn main() {
    let iters = 20000000;
    let acc = 0;
    let i = 0;
    while i < iters {
        let n = i % 10;
        let code = n;
        if n == 9 {
            code = -1;
        }
        acc = acc + match result_unit_probe(code) {
            Result::Ok(_) => 1,
            Result::Err(_) => 0,
        };
        i = i + 1;
    }
    let _ = print(format("%i", acc));
}
