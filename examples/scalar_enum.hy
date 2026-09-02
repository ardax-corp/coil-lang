// Scalar-backed enums: the runtime word is the `=` literal, the type is the enum.
// Constructors are namespaced: `Status::Ok` is not prelude `Result::Ok`.
// In expression position the value coerces to the backing (`int` here).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

#[repr(int)]
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn label(Status s) -> string {
    return match s {
        Status::Ok => "ok",
        default => "other",
    };
}

fn main() {
    let s = Status::Ok;
    write_all(stdout(), to_bytes(format("%s %i\n", label(s), s)));
}
