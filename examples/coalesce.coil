// ?? coalesce on Option; a Result takes .ok() first, which drops its error
// on purpose (#1878).
//
// @output bar,hi,7,9

use io::sync::print;
use string::format;

fn main() {
    let a = Option::None ?? "bar";
    let _ = print(format("%s,", a));
    let b = Option::Some("hi") ?? "bar";
    let _ = print(format("%s,", b));
    let c = Result::Err("boom").ok() ?? 7;
    let _ = print(format("%i,", c));
    let d = Result::Ok(9).ok() ?? 7;
    let _ = print(format("%i", d));
}
