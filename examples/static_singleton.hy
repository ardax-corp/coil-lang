use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
static let hits = 0;

class Counter {
    pub static count: int = 0,
    pub value: int,
}

impl Counter {
    pub fn bump() {
        Counter::count = Counter::count + 1;
        self.value = self.value + 1;
    }
}

fn main() {
    hits = hits + 1;
    Counter::count = Counter::count + 1;
    let c = new Counter(0);
    c.bump();
    write_all(stdout(), to_bytes(format("%i", hits)));
    write_all(stdout(), to_bytes(format("%i", Counter::count)));
    write_all(stdout(), to_bytes(format("%i", c.value)));
}
