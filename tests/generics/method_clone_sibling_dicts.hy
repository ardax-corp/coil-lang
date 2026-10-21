// A method clone (`Bag::add$inst$int`) calling a bounded sibling method
// that takes no type-parameter argument (`self.twice()`) passes it ground
// dictionaries: the generic body forwards its own, which a clone does not
// have (#1419).
use io::{stdout, write};
use string::{format, to_bytes};

// Output: 103 103

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

    fn twice() -> int {
        return self.slot(self.items[0]) + 100;
    }

    pub fn add(K k) -> int {
        self.items.push(k);
        return self.twice();
    }
}

fn main() {
    let b: Bag<int> = Bag::new();
    let x = b.add(10);
    let y = b.add(3);
    let _ = write(stdout(), to_bytes(format("%i %i", x, y)));
}
