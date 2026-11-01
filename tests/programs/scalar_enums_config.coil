// Scalar-backed enums with every backing type coerced into primitives, shown and held in an array.
// @output 200 fast 0.5 true Status::NotFound->404 lvl=2 [200, 404]\n
use io::sync::println;
use string::format;

#[repr(int)]
#[derive(Show, Eq, Ord)]
enum Status {
    Ok = 200,
    NotFound = 404,
}

#[repr(string)]
enum Mode {
    Fast = "fast",
    Slow = "slow",
}

#[repr(float)]
enum Ratio {
    Half = 0.5,
    Full = 1.0,
}

#[repr(bool)]
enum Switch {
    Off = false,
    On = true,
}

enum Level {
    Low = 1,
    Mid = 2,
    High = 3,
}

fn code(Status s) -> int {
    return s;
}

fn describe(Status s) -> string {
    return match s {
        Status::Ok => "Status::Ok",
        Status::NotFound => "Status::NotFound",
    } + format("->%i", s);
}

fn main() {
    let n: int = Status::Ok;
    let m: string = Mode::Fast;
    let r: float = Ratio::Half;
    let on: bool = Switch::On;
    let lvl = Level::Mid;
    let lv: int = lvl;
    let codes = [Status::Ok, Status::NotFound];
    let _ = println(
        format("%i %s %f %z %s lvl=%i %v", n, m, r, on, describe(Status::NotFound), lv, codes),
    );
    let _ = code(Status::Ok) < 300;
}
