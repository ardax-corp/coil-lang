// Expected: E0001 — trait instances use `impl Trait for Type`.
impl Show<int> {
    fn show(int x) -> string {
        return "";
    }
}

fn main() {}
