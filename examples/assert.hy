// prelude::test::assert — returns Result<(), string>.
//
// Output: ok,assertion failed,custom

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn check_ok(int n) {
    assert(n == 42)?;
    return "ok";
}

fn main() {
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s,",
                match check_ok(42) {
                    Result::Ok(v) => v,
                    Result::Err(e) => e,
                },
            ),
        ),
    );
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s,",
                match assert(false) {
                    Result::Ok(_) => "ok",
                    Result::Err(e) => e,
                },
            ),
        ),
    );
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match assert(1 == 0, "custom") {
                    Result::Ok(_) => "ok",
                    Result::Err(e) => e,
                },
            ),
        ),
    );
}
