// `#[derive(String)]` end-to-end: synthesized `to_string` is callable.
// Output: Color::Red
use io::{stdout, write};
use string::{format, to_bytes};

#[derive(String)]
enum Color {
    Red,
}

fn main() {
    write(stdout(), to_bytes(format("%s", Color::Red.to_string())));
}
