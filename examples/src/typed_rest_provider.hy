// Typed `T... xs` rest functions called from another module
// (tests/modules/typed_rest_across_modules.hy, coil-lang#1990).
fn total(int... xs) -> int {
    let t = 0;
    for x in xs {
        t = t + x;
    }
    return t;
}

fn joined(string sep, string... parts) -> string {
    let out = "";
    let first = true;
    for p in parts {
        if !first {
            out = out + sep;
        }
        out = out + p;
        first = false;
    }
    return out;
}

fn local_total() -> int {
    return total(1, 2, 3);
}
