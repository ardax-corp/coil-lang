// Built-in Result + Option — nested match with two Ok arms
// (inner Some vs None) plus Err. Exercises Phase 18A inner-pattern
// dispatch. Output: 420-1
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
    write_all(stdout(), to_bytes(format("%i", unwrap_result(Result::Ok(Option::Some(42))))));
    write_all(stdout(), to_bytes(format("%i", unwrap_result(Result::Ok(Option::None)))));
    write_all(stdout(), to_bytes(format("%i", unwrap_result(Result::Err("oops")))));
}
