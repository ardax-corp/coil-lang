// Expected: E0119 — `impl Ord for Score` needs the superclass instance `Eq<Score>` (#1716).
class Score {
    pub v: int,
}

impl Lt for Score {
    pub fn lt(Score a, Score b) -> bool {
        return a.v < b.v;
    }
}

impl Le for Score {
    pub fn le(Score a, Score b) -> bool {
        return a.v <= b.v;
    }
}

impl Gt for Score {
    pub fn gt(Score a, Score b) -> bool {
        return a.v > b.v;
    }
}

impl Ge for Score {
    pub fn ge(Score a, Score b) -> bool {
        return a.v >= b.v;
    }
}

impl Ord for Score {
}

fn main() {
    let b = new Score(1) < new Score(2);
}
