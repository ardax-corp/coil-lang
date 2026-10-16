// A `declare` bound to a local in one function does not refine `invoke`
// on a same-named parameter of another function: the parameter keeps the
// type its call site gives it (#1233). The handle `7` is not a loaded
// library, so each `declare` fails with `InvalidHandle` at run time.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Int;
use ffi::types::Float;

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn other(int lib) -> Result<int, Error> {
    let id = declare(lib, "g", (), Int)?;
    let n: int = invoke(lib, id, ())?;
    return n;
}

fn run() -> Result<(), Error> {
    let fn_id = declare(7, "f", (), Float)?;
    let _ = helper(fn_id)?;
    let _ = other(7)?;
}

fn invalid(Result<(), Error> r) -> bool {
    return match r {
        Result::Ok(_) => false,
        Result::Err(e) => match e.kind {
            ErrorKind::InvalidHandle => true,
            default => false,
        },
    };
}

test("a same-named local elsewhere leaves a parameter's refinement alone") {
    assert!(invalid(run()));
}
