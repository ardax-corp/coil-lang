// Expected: E0119 — only [byte] dynamic slices are allowed in let bindings.
fn main() {
    let xs: [string] = ["a"];
}
