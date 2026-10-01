// ANSI escape sequences in string literals (`\e`).
//
// Output: \e[31mred\e[0m

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    write_all(stdout(), to_bytes("\e[31mred\e[0m"));
}
