// Two `Result::Ok(Option::…)` arms before the `Result::Err` arm each pick
// their own inner variant.
// Output: 420-1
use io::{stdout, write};
use string::{format, to_bytes};

fn unwrap_result(Result r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Ok(Option::None) => 0,
        Result::Err(_) => -1,
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i", unwrap_result(Result::Ok(Option::Some(42))))));
    write(stdout(), to_bytes(format("%i", unwrap_result(Result::Ok(Option::None)))));
    write(stdout(), to_bytes(format("%i", unwrap_result(Result::Err("oops")))));
}
