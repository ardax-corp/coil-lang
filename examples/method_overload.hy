use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
class Counter {
    pub value: int,
}

impl Counter {
    pub fn bump(int by) -> int {
        self.value = self.value + by;
        return self.value;
    }

    pub fn bump() -> int {
        return self.bump(1);
    }
}

fn main() {
    let c = new Counter(10);
    write_all(stdout(), to_bytes(format("%i", c.bump())));
    write_all(stdout(), to_bytes(format("%i", c.bump(5))));
}
