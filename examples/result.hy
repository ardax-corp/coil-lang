// Built-in `Result` and `Option`: a nested match with two `Ok` arms (inner
// `Some` vs `None`) and an `Err` arm.
//
// Output: 420-1

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn unwrap_result(Result r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Ok(Option::None) => 0,
        Result::Err(_) => -1,
    };
}

fn main() {
    let _ = write_all(
        stdout(),
        to_bytes(format("%i", unwrap_result(Result::Ok(Option::Some(42))))),
    );
    let _ = write_all(stdout(), to_bytes(format("%i", unwrap_result(Result::Ok(Option::None)))));
    let _ = write_all(stdout(), to_bytes(format("%i", unwrap_result(Result::Err("oops")))));
}
