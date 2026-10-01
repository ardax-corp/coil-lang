// File round-trip via virtual `io` module.
// Writes two bytes, reads them back with read_to_end, prints length.
//
// Output: 2

use io::close;
use io::open;
use io::stdout;
use io::sync::read_to_end;
use io::sync::write_all;

use string::{format, to_bytes};

fn write_file(string path, Vec<byte> data) {
    let s = open(path, "w")?;
    write_all(s, data)?;
    close(s)?;
    return 0;
}

fn read_len(string path) {
    let s = open(path, "r")?;
    let buf = read_to_end(s)?;
    close(s)?;
    return len(buf);
}

fn run(string path, Vec<byte> data) {
    write_file(path, data)?;
    let n = read_len(path)?;
    return format("%i", n);
}

fn main() {
    let path = "coil_io_file_test.bin";
    let data = to_bytes("Hi");
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match run(path, data) {
                    Result::Ok(s) => s,
                    Result::Err(_) => "err",
                },
            ),
        ),
    );
}
