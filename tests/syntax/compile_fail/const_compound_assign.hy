// @expected E0107 — a `const` binding cannot be updated, also not by a
// compound assignment.
fn main() {
    const c = 1;
    c += 1;
}
