// Decode `Vec<byte>` → string with `from_bytes`, encode with `to_bytes`.
//
// Output: hello2

use io::stdout;
use io::sync::write_all;

use string::{format, from_bytes, to_bytes};

fn main() {
    // "hello" as ASCII bytes
    let hello = to_bytes("hello");
    write_all(
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
    write_all(stdout(), to_bytes(format("%i", len(back))));
}
