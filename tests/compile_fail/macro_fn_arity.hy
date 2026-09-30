// Expected: compile failure — `square!` takes one argument.
use fn_macros::square;

fn main() {
    let x = square!(1, 2);
}
