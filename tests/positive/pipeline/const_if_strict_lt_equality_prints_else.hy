// Const-fold `if` with strict `<` equality boundary takes else (Le vs Leq).
// Output: 0
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    if 5 < 5 {
        write(stdout(), to_bytes(format("%i", 1)));
    } else {
        write(stdout(), to_bytes(format("%i", 0)));
    }
}
