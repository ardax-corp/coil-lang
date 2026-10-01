// examples/modules.hy — modules and `use`.
//
// `use foo::sadge;` brings `sadge` (a function in `examples/src/foo/sadge.hy`)
// into scope. Modules are found on the search roots
// (`--root examples/src`); the function's fully qualified name is
// `foo::sadge::sadge`.
//
// Output: 1a4\n45\n

use foo::sadge;
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    sadge();
    write_all(stdout(), to_bytes(format("%x\n", 69)));
}
