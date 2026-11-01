// Decode `Vec<byte>` → string with `from_bytes`, encode with `to_bytes`.
//
// @output hello2

use io::stdout;
use io::sync::print;
use io::sync::write_all;

use string::{format, from_bytes, to_bytes};

fn main() {
    // "hello" as ASCII bytes
    let hello = to_bytes("hello");
    let _ = write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match from_bytes(hello) {
                    Result::Ok(s) => s,
                    Result::Err(_) => "err",
                },
            ),
        ),
    );

    let back = to_bytes("hi");
    let _ = print(format("%i", back.len()));
}
