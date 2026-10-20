// Expected: E0107 — `ro + x` calls `Add::add`, whose impl for `V` mutates `self`, so it cannot run on a `readonly` value (#1645).
class V {
    pub x: int,
}

impl Add for V {
    fn add(V self, V o) -> V {
        self.x = self.x + 1;
        return new V(self.x + o.x);
    }
}

fn main() {
    let ro = readonly new V(1);
    let s = ro + new V(2);
}
