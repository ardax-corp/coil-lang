// Output: 3
//
// A program whose only statics are interned trait dictionaries (built on
// first use) has no setup region: the prologue jumps straight to `main`.
use io::{stdout, write};
use string::{format, to_bytes};

class Bag<K> {
    items: Vec<K>,
}

impl Bag<K> {
    pub static fn new() -> Bag<K> {
        return new Bag(Vec::new());
    }
}

impl Bag<K: Eq + Hash> {
    fn slot(K k) -> int {
        return k.hash() % 7;
    }

    pub fn add(K k) -> int {
        self.items.push(k);
        return self.slot(k);
    }
}

fn main() {
    let b: Bag<int> = Bag::new();
    let _ = write(stdout(), to_bytes(format("%i", b.add(10))));
}
