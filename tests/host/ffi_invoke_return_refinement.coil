// `invoke` takes its return type (and variadic arity) from the `declare`
// that produced the function id, also when the id travels through a class
// field, a local copy, `self` in a method, or a parameter (positional or
// named) of a function declared before or after its caller; `ffi::types`
// tags also work by qualified path, and `ErrorKind` matches exhaustively.
// The handle `7`
// is not a loaded library, so each `declare` fails with `InvalidHandle` at
// run time; the cases pin what the checker accepts.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Int;
use ffi::types::Int32;
use ffi::types::Float;

extern struct Point {
    x: int32,
    y: int32,
};

class Api {
    pub id: int,
}

impl Api {
    pub fn load(int lib) -> Result<(), Error> {
        self.id = declare(lib, "f", (), Float)?;
    }

    pub fn call(int lib) -> Result<float, Error> {
        let f: float = invoke(lib, self.id, ())?;
        return f;
    }
}

fn struct_return(int lib) -> Result<(), Error> {
    let make_id = declare(lib, "make_point", (Int32, Int32), Point)?;
    let p = invoke(lib, make_id, (3, 4))?;
    let sum = p.x + p.y;
}

fn field_id(int lib) -> Result<(), Error> {
    let api = new Api(0);
    api.id = declare(lib, "f", (), Float)?;
    let f: float = invoke(lib, api.id, ())?;
    let _ = f;
}

fn local_copy_of_field(int lib) -> Result<(), Error> {
    let api = new Api(0);
    api.id = declare(lib, "f", (), Float)?;
    let fn_id = api.id;
    let f: float = invoke(lib, fn_id, ())?;
    let _ = f;
}

fn self_field_in_method(int lib) -> Result<(), Error> {
    let api = new Api(0);
    api.load(lib)?;
    let _ = api.call(lib)?;
}

fn early_helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn param_flow_forward(int lib) -> Result<(), Error> {
    let fn_id = declare(lib, "f", (), Float)?;
    let _ = early_helper(fn_id)?;
}

fn param_flow_named(int lib) -> Result<(), Error> {
    let fn_id = declare(lib, "f", (), Float)?;
    let _ = early_helper(id: fn_id)?;
}

fn param_flow_from_field(int lib) -> Result<(), Error> {
    let api = new Api(0);
    api.load(lib)?;
    let _ = late_helper(api.id)?;
}

fn late_helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn variadic_helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, (1, 2, 3))?;
    return f;
}

fn variadic_param(int lib) -> Result<(), Error> {
    let fn_id = declare(lib, "f", (Int,), Float, true)?;
    let _ = variadic_helper(fn_id)?;
}

fn variadic_field(int lib) -> Result<(), Error> {
    let api = new Api(0);
    api.id = declare(lib, "f", (Int,), Float, true)?;
    let f: float = invoke(lib, api.id, (1, 2, 3))?;
    let _ = f;
}

fn variadic_local_copy(int lib) -> Result<(), Error> {
    let api = new Api(0);
    api.id = declare(lib, "f", (Int,), Float, true)?;
    let fn_id = api.id;
    let f: float = invoke(lib, fn_id, (1, 2))?;
    let _ = f;
}

fn qualified_tags(int lib) -> Result<(), Error> {
    let sid = declare(lib, "sum", (ffi::types::Int, ffi::types::Int), ffi::types::Int)?;
    let n = invoke(lib, sid, (1, 2))?;
    let m: int = n;
}

fn kind_code(Result<(), Error> r) -> int {
    return match r {
        Result::Ok(_) => -1,
        Result::Err(e) => match e.kind {
            ErrorKind::LibraryNotFound => 0,
            ErrorKind::SymbolNotFound => 1,
            ErrorKind::ArityMismatch => 2,
            ErrorKind::Libffi => 3,
            ErrorKind::InvalidSignature => 4,
            ErrorKind::InvalidHandle => 5,
            ErrorKind::Unsupported => 6,
            ErrorKind::Other => 7,
        },
    };
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

test("struct return from declare") {
    assert!(invalid(struct_return(7)));
}

test("id through a field, a local copy and self") {
    assert!(invalid(field_id(7)));
    assert!(invalid(local_copy_of_field(7)));
    assert!(invalid(self_field_in_method(7)));
}

test("id through a parameter") {
    assert!(invalid(param_flow_forward(7)));
    assert!(invalid(param_flow_named(7)));
    assert!(invalid(param_flow_from_field(7)));
}

test("variadic declare allows extra invoke arguments") {
    assert!(invalid(variadic_param(7)));
    assert!(invalid(variadic_field(7)));
    assert!(invalid(variadic_local_copy(7)));
}

test("ffi::types tags by qualified path, without a use") {
    assert!(invalid(qualified_tags(7)));
}

test("every ErrorKind variant is matchable") {
    assert!(kind_code(qualified_tags(7)) == 5);
}
