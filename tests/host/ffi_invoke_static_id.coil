// An fn id `declare`d into a module-level `static` is passed to `invoke`
// directly, as a local or a field is (#1639). The handle `7` is not a
// loaded library, so `declare` and `invoke` fail with `InvalidHandle`.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Int;

static let fn_sum = 0;

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
    fn_sum = declare(lib, "sum", (Int, Int), Int)?;
    return fn_sum;
}

fn sum(int lib, int a, int b) -> Result<int, Error> {
    return invoke(lib, fn_sum, (a, b));
}

test("invoke takes its fn id from a static") {
    assert!(kind_of(load(7)) == "InvalidHandle");
    assert!(kind_of(sum(7, 1, 2)) == "InvalidHandle");
}
