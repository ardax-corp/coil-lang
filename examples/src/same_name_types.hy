// Helper module for tests/modules/same_name_types_across_modules.hy (#957):
// a class and an enum that share their names with the test file's own.

use string::format;

#[derive(Show, Eq)]
class Reader {
    pub n: int,
}

#[derive(Show, Eq)]
enum Mode {
    Fast,
    Slow,
}

fn make_reader(int n) -> Reader {
    return new Reader(n);
}

fn reader_n(Reader r) -> int {
    return r.n;
}

fn fast() -> Mode {
    return Mode::Fast;
}

fn show_reader(int n) -> string {
    return format("%v", new Reader(n));
}

fn is_fast(Mode m) -> bool {
    return match m {
        Mode::Fast => true,
        Mode::Slow => false,
    };
}
