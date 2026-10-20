// Expected: E0107 — an `Option` field read through a `readonly` holder hands back a `readonly Point` (#1552).
class Point {
    pub x: int,
}

class Holder {
    pub p: Option<Point>,
}

fn main() {
    let h = readonly new Holder(Option::Some(new Point(1)));
    match h.p {
        Option::Some(q) => {
            q.x = 5;
        },
        Option::None => {},
    }
}
