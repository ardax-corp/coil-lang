// Flags: --comptime-read tests/comptime/data
// Expected: E0134 — the file is outside the granted directory.
use io::file::read_text;

const TEXT = read_text("tests/comptime/const_values.hy") ?? "";

fn main() {}
