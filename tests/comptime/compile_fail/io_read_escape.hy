// @flags --comptime-read tests/comptime/data
// @expected E0134 — `..` does not lead out of the granted directory.
use io::file::read_text;

const TEXT = read_text("tests/comptime/data/../const_values.hy").ok() ?? "";

fn main() {}
