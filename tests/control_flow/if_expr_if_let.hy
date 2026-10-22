// `if let … else …` is an expression too: the pattern's bindings are in
// scope in the first branch only.
// @output 42 0
use io::{stdout, write};
use string::{format, to_bytes};

fn get(Option<int> o) -> int {
    return if let Option::Some(v) = o {
        v
    } else {
        0
    };
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i %i", get(Option::Some(42)), get(Option::None))));
}
