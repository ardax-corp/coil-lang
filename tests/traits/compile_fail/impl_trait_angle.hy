// Expected: E0001 — `impl Show<int>` is not instance syntax; an instance is
// written `impl Trait for Type`.
impl Show<int> {
    fn show(int x) -> string {
        return "";
    }
}

fn main() {}
