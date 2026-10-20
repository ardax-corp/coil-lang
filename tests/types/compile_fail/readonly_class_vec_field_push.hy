// Expected: E0107 — a readonly class's fields read `readonly`, so its `Vec` cannot grow through it (#1558).
readonly class Bag {
    pub items: Vec<int>,
}

impl Bag {
    pub fn add(int n) {
        self.items.push(n);
    }
}

fn main() {
    let b = new Bag(Vec::new());
    b.add(1);
}
