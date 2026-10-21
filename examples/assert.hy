// tests::assert! — a failed condition returns `Err` from the enclosing
// function: the given message, or one naming the condition.
//
// Output: ok,Assertion: 'n == 42' failed,custom

use tests::assert;
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn check(int n) -> Result<string, string> {
    assert!(n == 42);
    return Result::Ok("ok");
}

fn check_with_message(int n) -> Result<string, string> {
    assert!(n == 42, "custom");
    return Result::Ok("ok");
}

fn show(Result<string, string> r) -> string {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => e,
    };
}

fn main() {
    let line = show(check(42)) + "," + show(check(1)) + "," + show(check_with_message(1));
    let _ = write_all(stdout(), to_bytes(line));
}
