// Expected: E0126 — fn drop is not a trait method.
trait Closer {
    fn drop() {}
}

fn main() {}
