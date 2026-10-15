// Expected: E0128 — private field outside impl.
class Box {
    n: int,
}

fn main() {
    let b = new Box(1);
    let _ = b.n;
}
