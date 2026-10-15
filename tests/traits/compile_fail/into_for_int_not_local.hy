// Expected: E0119 — orphan instance: `Into` is not defined here and `int` is not a
// local type, even though `Wrapper` is.
class Wrapper {
    pub v: int,
}

impl Into<Wrapper> for int {
    fn into(int x) -> Wrapper {
        return new Wrapper(x);
    }
}

fn main() {}
