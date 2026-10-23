// Read two files concurrently as tasks. An IO wait suspends only its own
// task, so the other one keeps running; the scope waits for both.
//
// @output ok

use io::open;
use io::close;
use io::wait_readable;
use io::sync::print;
use io::sync::read_to_end;
use task::{scope, Scope, TaskError};

fn slurp(string path) -> Result<int, IoError> {
    let s = open(path, "r")?;
    wait_readable(s)?;
    let bytes = read_to_end(s)?;
    close(s)?;
    return Result::Ok(bytes.len());
}

fn size(Result<Result<int, IoError>, TaskError> r) -> int {
    return match r {
        Result::Ok(Result::Ok(n)) => n,
        default => -1,
    };
}

fn verdict(bool ok) -> string {
    if ok {
        return "ok";
    }
    return "failed";
}

fn main() {
    let r = scope(fn (Scope s) {
        let a = s.spawn(fn () => slurp("/etc/hosts"));
        let b = s.spawn(fn () => slurp("/etc/passwd"));
        size(a.join()) > 0 && size(b.join()) > 0
    });
    let text = match r {
        Result::Ok(ok) => verdict(ok),
        default => "failed",
    };
    let _ = print(text);
}
