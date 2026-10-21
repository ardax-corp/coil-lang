// Output: 4,1,1
use io::{stdout, write};
use string::{format, to_bytes};

fn parse(int value) {
    if value < 0 {
        raise "bad";
    }
    return value;
}

fn inc(int value) {
    let parsed = parse(value)?;
    return parsed + 1;
}

fn main() {
    let ok = match parse(4) {
        Result::Ok(value) => value,
        Result::Err(_) => -1,
    };
    let bad = match inc(-1) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let direct_bad = match parse(-1) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let _ = write(stdout(), to_bytes(format("%i,%i,%i", ok, bad, direct_bad)));
}
