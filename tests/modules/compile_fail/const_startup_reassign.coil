// @expected E0107 — a module `const` computed by a call cannot be reassigned.
fn version() -> string {
    return "1.0";
}

const VERSION = version();

fn main() {
    VERSION = "2.0";
}
