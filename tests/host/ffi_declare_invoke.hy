// `declare` and `invoke` lower on direct MIR: the signature's tags are
// constants, an argument naming a function is its code pointer, and the
// boxed `Result` the VM pushes is matched as it comes back. With no
// library loaded (a `dload` needs a grant), both fail with `InvalidHandle`.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Callback;
use ffi::types::Int;

fn doubler(int x) -> int {
    return x * 2;
}

fn kind_of(Result<int, Error> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e.kind {
            ErrorKind::InvalidHandle => "InvalidHandle",
            default => "other",
        },
    };
}

test("declare on a value that is not a library handle") {
    assert!(kind_of(declare(7, "sum", (Int, Int), Int)) == "InvalidHandle");
}

test("invoke with a callback on a value that is not a library handle") {
    let f = 0;
    assert!(kind_of(invoke(7, f, (doubler, 21))) == "InvalidHandle");
}

test("invoke's error message") {
    let f = 0;
    let msg = match invoke(7, f, (1, 2)) {
        Result::Ok(_) => "",
        Result::Err(e) => e.message,
    };
    assert!(len(msg) > 0);
}
