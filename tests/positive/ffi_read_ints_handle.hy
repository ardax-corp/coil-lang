// `ffi::read_ints` reads native memory only through a `dload` handle: any
// other value is an `InvalidHandle` error, and a bad count is refused before
// anything is read.
use ffi::{Error, ErrorKind, read_ints};

fn kind_of(Result<Vec<int>, Error> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e.kind {
            ErrorKind::InvalidHandle => "InvalidHandle",
            default => "other",
        },
    };
}

test("a non-handle is InvalidHandle") {
    assert(kind_of(read_ints(0, 0, 1)) == "InvalidHandle")?;
    assert(kind_of(read_ints(12345, 8, 2)) == "InvalidHandle")?;
}
