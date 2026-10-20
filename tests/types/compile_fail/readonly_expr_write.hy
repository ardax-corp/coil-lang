// Expected: E0107 — `readonly p` seals any expression, not just literals (#1558).
class Point {
    pub x: int,
}

fn main() {
    let p = new Point(1);
    let r = readonly p;
    r.x = 2;
}
