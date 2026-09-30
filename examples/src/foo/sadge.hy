// examples/src/foo/sadge.hy — namespace demo
//
// This file is at `examples/src/foo/sadge.hy`. Its
// namespace is `foo::sadge` (path relative to the
// `src` root, with `.hy` stripped and `/` replaced
// with `::`). Its top-level function `sadge()` has
// the fully qualified name `foo::sadge::sadge`.

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn sadge() {
    write_all(stdout(), to_bytes(format("%x\n", 420)));
}
