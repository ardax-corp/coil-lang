// A `static` holding `declare`'s unmatched `Result` is passed to `invoke`
// as its function id, as a local holding one is (#1731). The handle `7` is
// not a loaded library, so each `invoke` fails with `InvalidHandle`.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Float;

static let fn_static = declare(7, "f", (), Float);

fn invalid(Result<float, Error> r) -> bool {
    return match r {
        Result::Ok(_) => false,
        Result::Err(e) => match e.kind {
            ErrorKind::InvalidHandle => true,
            default => false,
        },
    };
}

fn call_static(int lib) -> Result<float, Error> {
    let f: float = invoke(lib, fn_static, ())?;
    return f;
}

fn call_local(int lib) -> Result<float, Error> {
    let id = declare(lib, "f", (), Float);
    let f: float = invoke(lib, id, ())?;
    return f;
}

test("invoke takes a static's unmatched declare Result as its id") {
    assert!(invalid(call_local(7)));
    assert!(invalid(call_static(7)));
}
