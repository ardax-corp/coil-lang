// ANSI escape sequences in string literals (`\e`).
//
// @output \e[31mred\e[0m

use io::sync::print;

fn main() {
    let _ = print("\e[31mred\e[0m");
}
