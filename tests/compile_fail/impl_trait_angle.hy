// Expected: parse failure — trait instances use `impl Trait for Type`.
impl Show<int> {
    fn show(int x) -> string {
        return "";
    }
}

fn main() {}
