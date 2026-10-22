// @expected E0119 — `to_missing!`'s output calls `missing!`, which its provider
// neither declares nor imports.
use fn_macros_imported::to_missing;

fn main() {
    let x = to_missing!(1);
}
