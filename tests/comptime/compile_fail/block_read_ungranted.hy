// Expected: E0119 — a `comptime` block reads files only under a granted
// `--comptime-read` directory.
use io::file::read_text;
use comptime_gen::adder;

comptime {
    let text = read_text("tests/comptime/data/greeting.txt") ?? "";
    return adder("f", text.len());
}

fn main() {}
