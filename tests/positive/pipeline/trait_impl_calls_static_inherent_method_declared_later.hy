// COI-115: static inherent helper used from a trait instance.
// Output: 7
use io::{stdout, write};
use string::{format, to_bytes};

class ItemBox {
    pub v: int,
}

class ItemBoxIter {
    pub i: int,
}

impl ItemBox {
    pub static fn make_iter(ItemBox m) -> ItemBoxIter {
        return new ItemBoxIter(m.v);
    }
}

impl IntoIterator for ItemBox {
    type Item = int;
    type IntoIter = ItemBoxIter;
    pub fn into_iter(ItemBox m) -> ItemBoxIter {
        return ItemBox::make_iter(m);
    }
}

impl Iterator for ItemBoxIter {
    type Item = int;
    pub fn next(ItemBoxIter it) -> Option<int> {
        if it.i == 0 {
            it.i = 1;
            return Option::Some(7);
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
