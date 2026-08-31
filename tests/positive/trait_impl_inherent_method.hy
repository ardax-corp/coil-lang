// Expected output: 1
//
// Trait-instance methods may call inherent methods on the same type (COI-115).

use io::{stdout, write};
use string::{format, to_bytes};

class ItemBox {
    pub v: int,
}

class ItemBoxIter {
    pub i: int,
}

impl ItemBox {
    pub fn iter() -> ItemBoxIter {
        return new ItemBoxIter(self.v);
    }
}

impl IntoIterator for ItemBox {
    type Item = int;
    type IntoIter = ItemBoxIter;
    pub fn into_iter(ItemBox m) -> ItemBoxIter {
        return m.iter();
    }
}

impl Iterator for ItemBoxIter {
    type Item = int;
    pub fn next(ItemBoxIter it) -> Option<int> {
        if it.i == 0 {
            it.i = 1;
            return Option::Some(1);
        }
        return Option::None;
    }
}

fn main() {
    let b = new ItemBox(0);
    for x in b {
        write(stdout(), to_bytes(format("%i", x)));
    }
}
