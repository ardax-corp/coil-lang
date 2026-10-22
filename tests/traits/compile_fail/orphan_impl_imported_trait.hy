// @expected E0119 — orphan instance: `Foreign` comes from another module and
// `int` is not a local type.
use ns_iface::Foreign;

impl Foreign for int {
    pub fn id(int x) -> int {
        return x;
    }
}

fn main() {}
