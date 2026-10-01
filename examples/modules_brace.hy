// examples/modules_brace.hy — brace-group imports from a module file.
//
// `examples/src/math.hy` exports `add` and `mul`. Brace syntax imports
// both in one statement; the resolver falls back to the module file
// (`math.hy`) because there is no `math/add.hy` / `math/mul.hy`.
//
// Output: 1242

use math::{add, mul};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    write_all(stdout(), to_bytes(format("%i", add(5, 7))));
    write_all(stdout(), to_bytes(format("%i", mul(6, 7))));
}
