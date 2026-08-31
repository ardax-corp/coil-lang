// Expected: compile failure — private field outside impl (E0128).
class Box {
    n: int,
}

fn main() {
    let b = new Box(1);
    let _ = b.n;
}
