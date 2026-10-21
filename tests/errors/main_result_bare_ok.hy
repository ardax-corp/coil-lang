// A bare `Ok(..)` returned from a `main` whose `Result` error type is a
// string lowers (#1738); see main_result_err_cli.rs for the `Err` side.
// Output: ok
use io::{stdout, write};
use string::to_bytes;

fn main() -> Result<int, string> {
    let _ = write(stdout(), to_bytes("ok"));
    return Ok(0);
}
