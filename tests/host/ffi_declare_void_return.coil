// `Void` as a `declare` return type is an FFI type tag, not a `()` value:
// the unit-const fold (#1798) must leave it a global so the signature
// lowers. The handle `7` is not a loaded library, so `declare` fails
// with `InvalidHandle`.
use tests::assert;
use ffi::declare;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Int;
use ffi::types::Void;

fn kind_of(Result<int, Error> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e.kind {
            ErrorKind::InvalidHandle => "InvalidHandle",
            default => "other",
        },
    };
}

fn load(int lib) -> Result<int, Error> {
    return declare(lib, "free", (Int,), Void);
}

test("declare takes a Void return type") {
    assert!(kind_of(load(7)) == "InvalidHandle");
}
