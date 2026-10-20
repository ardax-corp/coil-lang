// Expected: E0107 — `Result::Ok(p)` with a `readonly` `p` does not convert to `Result<Point, string>` (#1552).
class Point {
    pub x: int,
}

fn main() {
    let p = readonly new Point(1);
    let r: Result<Point, string> = Result::Ok(p);
}
