// @expected E0102 — a `comptime` block returns `Code`.
comptime {
    return 3;
}

fn main() {}
