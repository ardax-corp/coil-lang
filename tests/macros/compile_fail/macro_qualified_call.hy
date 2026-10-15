// Expected: E0001 — a macro call is a bare name (`square!(…)`); `m::square!(…)`
// does not parse.
use fn_macros::square;

fn main() {
    let x = fn_macros::square!(3);
}
