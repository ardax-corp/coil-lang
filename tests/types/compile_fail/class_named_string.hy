// Expected: E0200 — `String` is the built-in `string` (built-in type names
// match in any letter case); a class cannot take the name (#1673).
class String {
    pub s: string,
}

impl Show for String {
    pub fn show(String x) -> string {
        return x.s;
    }
}

fn main() {
    let v = new String("hi");
    let _ = show(v);
}
