// Trait-instance methods may call inherent methods on the same type (COI-115).

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

test("trait instance method calls an inherent method") {
    let n = 0;
    let total = 0;
    for x in new ItemBox(0) {
        n = n + 1;
        total = total + x;
    }
    assert(n == 1)?;
    assert(total == 1)?;
}
