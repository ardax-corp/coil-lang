// Expected: compile failure — [int; 2] does not unify with [int; 8]
fn main() {
    let a: [int; 2] = [1, 2];
    let b: [int; 8] = a;
}
