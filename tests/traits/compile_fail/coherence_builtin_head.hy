// Expected: E0119 — orphan instance: this module defines neither `Show` nor a nominal
// type in the head `(int, int)`.
impl Show for (int, int) {
    fn show((int, int) p) -> string {
        return "p";
    }
}

fn main() {}
