// A string returned through a `forall T: C. …` parameter stays a live
// heap reference in its local slot after a call with a different
// constraint's dictionary, at every optimization level (#1530).
use io::{stdout, write};
use string::{format, to_bytes};

// Output: 40|k

trait Tagged<T> {
    fn tag_of(T x) -> int {}
}

impl Tagged for int {
    pub fn tag_of(int x) -> int {
        return x * 10;
    }
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

fn tags(forall T: Tagged. T -> int f, int n) -> int {
    return f(n);
}

fn main() {
    let c = tags(tagged, 4);
    let d = apply(konst, 3);
    let _ = write(stdout(), to_bytes(format("%i|%s\n", c, d)));
}
