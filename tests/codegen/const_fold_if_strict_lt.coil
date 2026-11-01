// Const-fold `if` with strict `<` equality boundary takes else (Le vs Leq).
// @output 0
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    if 5 < 5 {
        let _ = write(stdout(), to_bytes(format("%i", 1)));
    } else {
        let _ = write(stdout(), to_bytes(format("%i", 0)));
    }
}
