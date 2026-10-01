// Nested IO HostInvoke as the first of two args: `write_all(open(...), buf)`.
// Regression for emit_io_host_invoke when outer arity > 1.
//
// Output: 2

use io::close;
use io::open;
use io::stdout;
use io::sync::read_to_end;
use io::sync::write_all;

use string::{format, to_bytes};

fn main() {
    let path = "coil_io_nested_write.bin";
    let z: byte = 0;
    let a: byte = 120;
    let b: byte = 121;
    let payload = Vec::from([a, b]);
    // Nested arity>1: outer native id must precede nested open()'s HostInvoke.
    write_all(open(path, "w")?, payload)?;
    let r = open(path, "r")?;
    let got = match read_to_end(r) {
        Result::Ok(buf) => buf,
        Result::Err(_) => Vec::from([z]),
    };
    close(r)?;
    write_all(stdout(), to_bytes(format("%i", len(got))));
}
