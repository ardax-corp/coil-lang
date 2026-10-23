// ?? coalesce on Option and Result (Result Err is swallowed).
//
// @output bar,hi,7,9

use io::sync::print;
use string::format;

fn main() {
    let a = Option::None ?? "bar";
    let _ = print(format("%s,", a));
    let b = Option::Some("hi") ?? "bar";
    let _ = print(format("%s,", b));
    let c = Result::Err("boom") ?? 7;
    let _ = print(format("%i,", c));
    let d = Result::Ok(9) ?? 7;
    let _ = print(format("%i", d));
}
