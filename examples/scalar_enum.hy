// Scalar-backed enums: the runtime word is the `=` literal, the type is the enum.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

#[repr(int)]
enum Status {
    Success = 200,
    NotFound = 404,
}

fn label(Status s) -> string {
    return match s {
        Status::Success => "ok",
        default => "other",
    };
}

fn main() {
    let s = Status::Success;
    write_all(stdout(), to_bytes(format("%s %i\n", label(s), s.value)));
}
