// @output abc|a(bc)|1234|[x][y][z]|<>|e1e2e3|xyxyxy
// A chain of `+` on strings is built as one string from all its parts:
// each part still runs once, left to right, however it is grouped.
use io::{stdout, write};
use string::to_bytes;

class Log {
    pub text: string,
}

fn note(Log log, string s) -> string {
    log.text = log.text + "e" + s;
    return "";
}

fn main() {
    let a = "a";
    let b = "b";
    let c = "c";
    let flat = a + b + c;
    let grouped = a + "(" + (b + c) + ")";
    let nums = show(1) + show(2) + (show(3) + show(4));
    let out = "";
    for s in ["x", "y", "z"] {
        out = out + "[" + s + "]";
    }
    let empty = "<" + "" + "" + ">";
    let log = new Log("");
    let _ = note(log, "1") + note(log, "2") + note(log, "3");
    let rep = "";
    let i = 0;
    while i < 3 {
        rep = rep + "x" + "y";
        i += 1;
    }
    let text = flat + "|" + grouped + "|" + nums + "|" + out + "|" + empty + "|" + log.text + "|" +
               rep;
    let _ = write(stdout(), to_bytes(text));
}
