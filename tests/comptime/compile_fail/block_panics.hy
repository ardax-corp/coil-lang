// Expected: E0119 — a panic in a `comptime` block fails the build.
comptime {
    panic "no code today";
}

fn main() {}
