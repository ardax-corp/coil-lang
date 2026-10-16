// A bounded generic passed to a `forall T: C. …` parameter gets the
// constraint's dictionary at each call inside the callee (#1188).
use io::{stdout, write};
use string::{format, to_bytes};

// Output: 1|2.5|true|40|k|7 true

trait Tagged<T> {
    fn tag_of(T x) -> int {}
}

impl Tagged for int {
    pub fn tag_of(int x) -> int {
        return x * 10;
    }
}

fn text<T: Show>(T x) -> string {
    return show(x);
}

fn tagged<T: Tagged>(T x) -> int {
    return tag_of(x);
}

fn konst<T>(T x) -> string {
    return "k";
}

fn apply(forall T: Show. T -> string f, int n) -> string {
    return f(n);
}

fn both(forall T: Show. T -> string f, float x, bool b) -> string {
    return f(x) + "|" + f(b);
}

fn tags(forall T: Tagged. T -> int f, int n) -> int {
    return f(n);
}

fn main() {
    let a = apply(text, 1);
    let b = both(text, 2.5, true);
    let c = tags(tagged, 4);
    let d = apply(konst, 3);
    write(
        stdout(),
        to_bytes(format("%s|%s|%i|%s|%s\n", a, b, c, d, apply(text, 7) + " " + text(true))),
    );
}
