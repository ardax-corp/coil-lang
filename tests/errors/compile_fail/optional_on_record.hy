// @expected E0116 — a plain record is not an Option; use `.` instead of `?.`.
fn main() {
    let d = { v: 1 };
    let r = d?.v;
}
