// P4: natural Ok/Ok/Err arm order (Err last) must not panic at codegen.
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
