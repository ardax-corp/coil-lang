// @expected E0001 — functions are declared at the top level only.
fn main() {
    fn inner() -> int {
        return 1;
    }
    let r = inner();
}
