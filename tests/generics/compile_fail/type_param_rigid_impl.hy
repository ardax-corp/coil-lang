// Expected: E0119 — an impl's type parameter is rigid in every method (#801).
class Keys<K> {
    pub keys: Vec<K>,
}

impl Keys<K: Ord> {
    pub fn below(int idx) -> bool {
        return idx < self.keys[0];
    }

    pub fn first() -> K {
        return self.keys[0];
    }
}

fn main() {}
