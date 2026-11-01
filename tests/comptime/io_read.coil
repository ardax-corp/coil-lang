// @flags --comptime-read tests/comptime/data --comptime-read comptime_io=tests/comptime/data
// A `const`, a `comptime` expression and a macro read files while compiling,
// within the granted directory; the program itself needs no `--allow-read`.
use io::file::read_text;
use comptime_io::file_text;
use tests::assert;

const GREETING = read_text("tests/comptime/data/greeting.txt").ok() ?? "";

test("a const reads a file") {
    assert!(GREETING == "hello from a file\n");
}

test("a comptime expression reads a file") {
    let text = comptime read_text("tests/comptime/data/greeting.txt");
    assert!((text.ok() ?? "") == "hello from a file\n");
}

test("a macro reads a file") {
    assert!(file_text!(greeting) == "hello from a file\n");
}
