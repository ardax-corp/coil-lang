// Expected: E0119 — packing a value as `Describe` needs a `Describe` instance for its type.
trait Describe<D> {
    fn describe(D self) -> string;
}

class Sq {
    pub side: int,
}

fn say(Describe d) -> string {
    return describe(d);
}

fn main() {
    say(new Sq(1));
}
