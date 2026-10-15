// Expected: E0102 — `<` orders numbers and `Ord` types; `string` has no
// ordering instance.
fn main() {
    let lt = "a" < "b";
}
