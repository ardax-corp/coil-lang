// Expected: E0128 — private method outside impl.
class Box {
    n: int,
}

impl Box {
    fn secret() -> int {
        return self.n;
    }
}

fn main() {
    let b = new Box(1);
    let _ = b.secret();
}
