// `io::fs::exists(".")` returns `Ok` for the working directory.
// @output 1
use io::fs::exists;
use io::stdout;
use io::write;
use string::{format, to_bytes};

fn dot_ok() -> int {
    return match exists(".") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", dot_ok())));
}
