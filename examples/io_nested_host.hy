// Nested IO HostInvoke: `read_to_end(open(...))` must pass the stream, not
// the native id, into MakeTuple (regression for emit_io_host_invoke arg order).
use io::close;
use io::open;
use io::stdout;
use io::sync::read_to_end;
use io::sync::write_all;

use string::{format, to_bytes};

fn main() {
    let path = "coil_io_nested_host.bin";
    let z: byte = 0;
    let a: byte = 97;
    let b: byte = 98;
    let c: byte = 99;
    let payload = Vec::from([a, b, c]);
    let w = open(path, "w")?;
    write_all(w, payload)?;
    close(w)?;
    let got = match read_to_end(open(path, "r")?) {
        Result::Ok(buf) => buf,
        Result::Err(_) => Vec::from([z]),
    };
    write_all(stdout(), to_bytes(format("%i", len(got))));
}
