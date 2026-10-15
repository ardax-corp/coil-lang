// Expected: E0413 — pushing onto a `Vec` the function was passed is `mutate`.
pure fn add(Vec<int> xs) {
    xs.push(1);
}

fn main() {
    let v: Vec<int> = Vec::new();
    add(v);
}
