// `invoke` takes its return type (and variadic arity) from the `declare`
// that produced the function id also when the id is a module `static`:
// one assigned in a function written before or after the `invoke`, a
// `static` or `const` initialised from `declare`, a local copy, and one
// passed on as a parameter (#1667).
// The handle `7` is not a loaded library, so each `declare` and `invoke`
// fails with `InvalidHandle`; the cases pin what the checker accepts.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Int;
use ffi::types::Float;

static let fn_late = 0;

static let fn_early = 0;

static let fn_var = 0;

static let fn_init = match declare(7, "f", (), Float) {
    Result::Ok(id) => id,
    Result::Err(_) => 0,
};

const FN_CONST = match declare(7, "f", (), Float) {
    Result::Ok(id) => id,
    Result::Err(_) => 0,
};

fn call_late(int lib) -> Result<float, Error> {
    let f: float = invoke(lib, fn_late, ())?;
    return f;
}

fn load_late(int lib) -> Result<(), Error> {
    fn_late = declare(lib, "f", (), Float)?;
}

fn load_early(int lib) -> Result<(), Error> {
    fn_early = declare(lib, "f", (), Float)?;
}

fn call_early(int lib) -> Result<float, Error> {
    let f: float = invoke(lib, fn_early, ())?;
    return f;
}

fn call_variadic(int lib) -> Result<float, Error> {
    fn_var = declare(lib, "f", (Int,), Float, true)?;
    let f: float = invoke(lib, fn_var, (1, 2, 3))?;
    return f;
}

fn call_init(int lib) -> Result<float, Error> {
    let f: float = invoke(lib, fn_init, ())?;
    return f;
}

fn call_const(int lib) -> Result<float, Error> {
    let f: float = invoke(lib, FN_CONST, ())?;
    return f;
}

fn call_copy(int lib) -> Result<float, Error> {
    let id = fn_init;
    let f: float = invoke(lib, id, ())?;
    return f;
}

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(7, id, ())?;
    return f;
}

fn via_param() -> Result<float, Error> {
    return helper(fn_early);
}

fn call_shadowed(int lib) -> Result<int, Error> {
    let fn_early = 0;
    let n: int = invoke(lib, fn_early, ())?;
    return n;
}

fn failed<T>(Result<T, Error> r) -> bool {
    return match r {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
}

fn invalid(Result<float, Error> r) -> bool {
    return match r {
        Result::Ok(_) => false,
        Result::Err(e) => match e.kind {
            ErrorKind::InvalidHandle => true,
            default => false,
        },
    };
}

test("invoke refines its return type through a static") {
    assert!(failed(load_late(7)));
    assert!(failed(load_early(7)));
    assert!(invalid(call_late(7)));
    assert!(invalid(call_early(7)));
    assert!(invalid(call_init(7)));
    assert!(invalid(call_const(7)));
    assert!(invalid(call_copy(7)));
    assert!(invalid(via_param()));
}

test("a variadic declare in a static allows extra invoke arguments") {
    assert!(invalid(call_variadic(7)));
}

test("a local shadowing the static is not refined") {
    assert!(failed(call_shadowed(7)));
}
