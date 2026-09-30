// CPU: deep recursion without auto-par binary shape (cross-lang fair bench).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

#[max_depth(4096)]
fn tak(int x, int y, int z) -> int {
    if y >= x {
        return z;
    }
    return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y));
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", tak(18, 12, 6))));
}
