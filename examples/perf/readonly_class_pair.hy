// Hit bench for #1562 step 2: a two-float readonly class returned from a
// function the inliner keeps (it recurses) moves as two words on the
// direct CALL/RETURN, like a `(float, float)` tuple, instead of an object
// per return.
// Checksum: 45000150000.0
use io::sync::print;
use string::format;

readonly class Vec2 {
    pub x: float,
    pub y: float,
}

fn turn(Vec2 p, int n) -> Vec2 {
    if n == 0 {
        return new Vec2(p.y, 0.0 - p.x);
    }
    let q = turn(p, n - 1);
    return new Vec2(q.y + 1.0, 0.0 - q.x);
}

fn main() {
    let acc = 0.0;
    for i in 0..300000 {
        let r = turn(new Vec2(i as float, 1.0), 8);
        acc = acc + r.x - r.y;
    }
    let _ = print(format("%f", acc));
}
