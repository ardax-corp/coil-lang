// Expected: E0101 — there is no free `push` function; append with `v.push(x)` on a `Vec`.
fn main() {
    let a = [1];
    push(a, 2);
}
