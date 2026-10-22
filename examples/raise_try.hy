// raise + ? with inferred Result return (catchable; not process abort — Q5).
//
// @output 10,neg

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn parse_pos(int n, int is_neg) {
    if is_neg == 1 {
        raise "neg";
    }
    return n;
}

fn double_pos(int n, int is_neg) {
    let v = parse_pos(n, is_neg)?;
    return v * 2;
}

fn show_i(Result r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn show_e(Result r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn main() {
    let _ = write_all(stdout(), to_bytes(format("%i,", show_i(double_pos(5, 0)))));
    let _ = write_all(stdout(), to_bytes(format("%s", show_e(double_pos(1, 1)))));
}
