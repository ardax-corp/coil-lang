// CPU/allocation workload for pointer-niche Option and unary Result pairs.

use io::sync::print;

use string::format;

fn maybe_text(int value) -> Option<string> {
    if value % 3 == 0 {
        return Option::Some("hit");
    }
    return Option::None;
}

fn checked_value(int value) {
    if value % 5 == 0 {
        raise "miss";
    }
    return value % 7;
}

fn main() {
    let total = 0;
    let value = 0;
    while value < 10000 {
        total = total + match maybe_text(value) {
            Option::Some(_) => 1,
            Option::None => 0,
        };
        total = total + match checked_value(value) {
            Result::Ok(result) => result,
            Result::Err(_) => -1,
        };
        value = value + 1;
    }
    let _ = print(format("%i", total));
}
