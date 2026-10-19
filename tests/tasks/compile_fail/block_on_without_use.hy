// Expected: E0101 — `block_on` is no prelude builtin any more; it is
// `task::block_on` (help names `use task::block_on;`).
gen fn one() -> int {
    return 1;
}

fn main() {
    let _ = block_on(one());
}
