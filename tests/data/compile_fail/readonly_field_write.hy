// Expected: E0107 — a `readonly` handle rejects external field writes.
class P {
    pub x: int,
}

fn main() {
    let p = readonly new P(1);
    p.x = 2;
}
