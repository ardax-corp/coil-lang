// EOF is Ok(None) from a non-blocking `read` on an empty file.
//
// Output: eof

use io::close;
use io::open;
use io::read;
use io::stdout;
use io::sync::write_all;

use string::{format, to_bytes};

fn make_empty(string path) {
    let s = open(path, "w")?;
    close(s)?;
    return 0;
}

fn read_once(string path) {
    let s = open(path, "r")?;
    let z: byte = 0;
    let buf = Vec::from([z, z, z, z]);
    let r = read(s, buf)?;
    close(s)?;
    return r;
}

fn describe(string path) {
    make_empty(path)?;
    let r = read_once(path)?;
    return match r {
        Option::None => "eof",
        Option::Some(_) => "data",
    };
}

fn main() {
    let path = "coil_io_eof_test.bin";
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match describe(path) {
                    Result::Ok(s) => s,
                    Result::Err(_) => "err",
                },
            ),
        ),
    );
}
