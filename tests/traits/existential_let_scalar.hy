// Output: 1|int 5|x
// A `let` annotated with a trait packs a scalar value too (#1183), for a
// builtin trait and a user trait alike.
use io::{stdout, write};
use string::{format, to_bytes};

trait D<T> {
    fn d(T self) -> string {}
}

impl D for int {
    fn d(int n) -> string {
        return "int " + show(n);
    }
}

fn main() {
    let b: Show = 1;
    let s = show(b);
    let a: D = 5;
    let c: Show = "x";
    write(stdout(), to_bytes(format("%s|%s|%s", s, d(a), show(c))));
}
