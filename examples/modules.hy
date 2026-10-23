// examples/modules.hy — modules and `use`.
//
// `use foo::sadge;` brings `sadge` (a function in `examples/src/foo/sadge.hy`)
// into scope. Modules are found on the search roots
// (`--root examples/src`); the function's fully qualified name is
// `foo::sadge::sadge`.
//
// @output 1a4\n45\n

use foo::sadge;
use io::sync::println;
use string::format;

fn main() {
    sadge();
    let _ = println(format("%x", 69));
}
