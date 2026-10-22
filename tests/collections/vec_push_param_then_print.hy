// A push onto a `Vec<int>` parameter reaches the caller's vector even when
// the callee then allocates and prints (#1381, lost at -O0).
// @output n1\n1 5 7\n
use io::sync;
use string::format;

fn push1(Vec<int> codes, int code) {
    codes.push(code);
    let k = format("n%i", code);
    let _ = io::sync::println(k);
}

fn push2(Vec<int> codes, Vec<string> names, int code) {
    codes.push(code);
    names.push(format("n%i", code));
}

fn main() {
    let c: Vec<int> = [];
    push1(c, 1);
    let d: Vec<int> = [5];
    let s: Vec<string> = [];
    push2(d, s, 7);
    let _ = io::sync::println(format("%i %i %i", c.len(), d[0], d[1]));
}
