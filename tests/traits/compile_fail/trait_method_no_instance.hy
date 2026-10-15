// Expected: E0119 — a trait method call on a type with no instance (`Describe<string>`).
trait Describe<D> {
    fn describe(D self) -> string;
}

impl Describe for int {
    fn describe(int n) -> string {
        return "int";
    }
}

fn main() {
    describe("text");
}
