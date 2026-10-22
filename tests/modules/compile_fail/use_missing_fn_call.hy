// @expected E0100 — calling a name the module does not define is a name error.
use math::nosuch;

fn main() {
    let x = nosuch(1);
}
