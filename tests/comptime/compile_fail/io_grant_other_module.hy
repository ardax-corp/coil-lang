// Flags: --comptime-read some_other_module=tests/comptime/data
// Expected: E0134 — a grant scoped to another module does not apply here.
use io::file::read_text;

const TEXT = read_text("tests/comptime/data/greeting.txt") ?? "";

fn main() {}
