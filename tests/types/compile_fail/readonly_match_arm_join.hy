// Expected: E0107 — a `match` whose later arm is `readonly` joins to a `readonly` value (#1552).
class Point {
    pub x: int,
}

fn main() {
    let p = readonly new Point(1);
    let q = match p.x {
        0 => new Point(0),
        default => p,
    };
    q.x = 7;
}
