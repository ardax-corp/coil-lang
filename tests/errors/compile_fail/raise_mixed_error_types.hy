// @expected E0117 or E0102 — one error type per function: `raise "s"` and `raise 1` conflict (reported as E0102 today).
fn f(int n) {
    if n > 0 {
        raise "s";
    }
    raise 1;
}

fn main() {
    let r = f(1);
}
