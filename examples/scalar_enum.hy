// Scalar-backed enums: the runtime word is the `=` literal, the type is the enum.
// Constructors are namespaced: `Status::Ok` is not prelude `Result::Ok`.
// In expression position the value coerces to the backing (`int` here).
// Show/String of a case is that backing (`Status.Ok` shows as `200`).
//
// @output ok 200 200\n

use io::sync::println;
use string::format;

#[repr(int)]
#[derive(Show, Eq, Ord, Hash)]
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
    let _ = println(format("%s %i %v", label(s), s, s));
}
