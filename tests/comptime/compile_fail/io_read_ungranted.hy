// Expected: E0134 — reading a file while compiling needs `--comptime-read`.
use io::file::read_text;

const TEXT = read_text("tests/comptime/data/greeting.txt") ?? "";

fn main() {}
