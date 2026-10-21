// Expected: E0119 — one scope may declare a type alias name once.
type A = int;

type A = string;

fn main() {}
