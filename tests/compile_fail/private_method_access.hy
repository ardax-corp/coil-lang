// Expected: compile failure — private method outside impl (E0128).
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
