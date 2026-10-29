// @expected E0102 — `type Item` declared after `next` still pins the trait's Option<Item> to Option<int> (#2086).
class P {
    pub n: int,
}

impl Iterator for P {
    fn next(P p) -> Option<string> {
        if p.n > 0 {
            p.n -= 1;
            return Option::Some("x");
        }
        return Option::None;
    }
    type Item = int;
}

fn main() {
    let p = new P(2);
    let _ = p.next();
}
