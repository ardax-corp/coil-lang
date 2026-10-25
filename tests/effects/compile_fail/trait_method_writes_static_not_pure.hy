// @expected E0413
// The impl writes a static, so a `uses {}` caller is refused (#2008).
use concrete_trait_methods::Loud;

fn quiet(string s) -> string uses {} {
    return s.loud();
}

fn main() {
    quiet("a");
}
