// Expected: E0107 — tuples are immutable; `t[0] = …` is not an assignment target.
fn main() {
    let t = (1, 2);
    t[0] = 5;
}
