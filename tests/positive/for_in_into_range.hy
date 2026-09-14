use io::{stdout, write};
use string::{format, to_bytes};

class Holder {
    pub start: int,
    pub end: int,
}

impl IntoIterator for Holder {
    type Item = int;
    type IntoIter = Range<int>;
    fn into_iter(Holder h) -> Range<int> {
        return h.start..h.end;
    }
}

fn main() {
    let s = 0;
    for x in new Holder(0, 4) {
        s = s + x;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
