// Nested typed lambdas must keep Fragment/Argument NodeIds lockstep with
// codegen (`assign_fn_arg_node_ids`); a desync surfaces as wrong results or
// compile failure rather than Identifier span prefer.
// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let add = fn (int x) => fn (int y) use (x) => x + y;
    let add40 = add(40);
    write(stdout(), to_bytes(format("%i", add40(2))));
}
