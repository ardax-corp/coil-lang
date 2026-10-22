// @expected E0001 — block comments nest, so one `*/` closes only the
// inner comment and the outer one runs to the end of the file.
fn main() {
    /* outer /* inner */ still open
    let x = 1;
}
