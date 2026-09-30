// ?? coalesce on Option and Result (Result Err is swallowed).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = Option::None ?? "bar";
    write_all(stdout(), to_bytes(format("%s,", a)));
    let b = Option::Some("hi") ?? "bar";
    write_all(stdout(), to_bytes(format("%s,", b)));
    let c = Result::Err("boom") ?? 7;
    write_all(stdout(), to_bytes(format("%i,", c)));
    let d = Result::Ok(9) ?? 7;
    write_all(stdout(), to_bytes(format("%i", d)));
}
