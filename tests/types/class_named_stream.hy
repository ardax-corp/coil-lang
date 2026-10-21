// A user `class Stream` is its own type next to `io`'s opaque `Stream`
// (#1673).
// Output: 3
use io::{stdout, write};
use string::{format, to_bytes};

class Stream {
    pub n: int,
}

fn get(Stream x) -> int {
    return x.n;
}

fn main() {
    write(stdout(), to_bytes(format("%i", get(new Stream(3)))));
}
