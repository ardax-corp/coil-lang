// Expected: E0101 — no compiler-provided `to_string` any more; call `show()`.
class Pt {
    pub x: int,
}

fn main() {
    let _ = new Pt(1).to_string();
}
