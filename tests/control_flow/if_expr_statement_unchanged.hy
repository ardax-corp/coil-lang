// A statement `if` keeps its behaviour next to the expression form: no
// `else` needed, bodies are statements, and no `;` after the `}`.
// @output 1 3
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let x = 0;
    if true {
        x = 1;
    }
    let y = 0;
    if x == 1 {
        y = 3;
    } else if x == 2 {
        y = 4;
    }
    let _ = write_all(stdout(), to_bytes(format("%i %i", x, y)));
}
