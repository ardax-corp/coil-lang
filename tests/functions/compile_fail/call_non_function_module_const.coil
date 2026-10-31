// @expected E0104 — `m::LIMIT(1)` calls another module's `const` that is an `int`, not a function (#1726).
use qualified_fn_value_provider;

fn main() {
    qualified_fn_value_provider::LIMIT(1);
}
