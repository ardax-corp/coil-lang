// A macro that reads a file while compiling, for `tests/comptime/`.
use macro::{Expr, Code, lit};
use io::file::read_text;

/// `file_text!(name)`: the text of `tests/comptime/data/<name>.txt` as a
/// string literal (`""` when it cannot be read).
macro file_text(Expr name) -> Code {
    let text = read_text("tests/comptime/data/" + name.src() + ".txt") ?? "";
    return lit(text);
}
