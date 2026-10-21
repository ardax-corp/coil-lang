// Flags: --comptime-read tests/comptime/data
// Expected: E0134 — `..` does not lead out of the granted directory.
use io::file::read_text;

const TEXT = read_text("tests/comptime/data/../const_values.hy") ?? "";

fn main() {}
