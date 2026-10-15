// A class field holding a payload enum reads back the stored variant.
// Output: 99
use io::{stdout, write};
use string::{format, to_bytes};

enum Status {
    Ready,
    Done(int),
}

class Box {
    pub status: Status,
}

impl Box {
    pub fn get() -> Status {
        return self.status;
    }
}

fn main() {
    let b = new Box(Status::Done(9));
    let s = b.get();
    write(
        stdout(),
        to_bytes(
            format(
                "%i",
                match s {
                    Status::Ready => 0,
                    Status::Done(v) => v,
                },
            ),
        ),
    );
    write(
        stdout(),
        to_bytes(
            format(
                "%i",
                match b.status {
                    Status::Ready => 0,
                    Status::Done(v) => v,
                },
            ),
        ),
    );
}
