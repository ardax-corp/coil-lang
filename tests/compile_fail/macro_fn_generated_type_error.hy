// Expected: compile failure — generated code has a type error, reported at
// the call.
use fn_macros_bad::{mistyped};

fn main() {
    mistyped!("text");
}
