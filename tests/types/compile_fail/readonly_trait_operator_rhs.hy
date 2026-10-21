// Expected: E0107 — `x + ro` passes `ro` to `Add::add`, whose impl for `V` mutates its parameter `o` (#1645).
class V {
    pub x: int,
}

impl Add for V {
    fn add(V self, V o) -> V {
        o.x = 0;
        return new V(self.x);
    }
}

fn main() {
    let ro = readonly new V(1);
    let s = new V(2) + ro;
}
