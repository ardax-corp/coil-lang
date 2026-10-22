// `if` as an expression: `let`, `return` and operands take the value of the
// taken branch; `else` is required there.
//
// @output small big

use io::{stdout, write};
use string::to_bytes;

fn size(int n) -> string {
    return if n < 10 {
        "small"
    } else {
        "big"
    };
}

fn main() {
    let sep = if true {
        " "
    } else {
        ""
    };
    let _ = write(stdout(), to_bytes(size(3) + sep + size(30)));
}
