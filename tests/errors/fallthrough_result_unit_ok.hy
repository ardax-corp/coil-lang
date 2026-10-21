// Output: 1
use io::{stdout, write};
use string::{format, to_bytes};

fn ok_unit() -> Result<(), string> {}

fn main() {
    let _ = write(
        stdout(),
        to_bytes(
            format(
                "%i",
                match ok_unit() {
                    Result::Ok(_) => 1,
                    Result::Err(_) => 0,
                },
            ),
        ),
    );
}
