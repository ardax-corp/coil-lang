// An fn id `declare`d into a class field by a method refines `invoke`
// outside the impl too: read from the object in another function, or
// passed on as an `int` parameter (#1232). The handle `7` is not a loaded
// library, so `declare` fails with `InvalidHandle` at run time.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::ErrorKind;
use ffi::types::Float;

class Api {
    pub id: int,
}

impl Api {
    pub fn load(int lib) -> Result<(), Error> {
        self.id = declare(lib, "f", (), Float)?;
    }
}

fn read(Api api) -> Result<float, Error> {
    let f: float = invoke(0, api.id, ())?;
    return f;
}

fn read_id(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn through_object() -> Result<(), Error> {
    let api = new Api(0);
    api.load(7)?;
    let _ = read(api)?;
}

fn through_param() -> Result<(), Error> {
    let api = new Api(0);
    api.load(7)?;
    let _ = read_id(api.id)?;
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

test("a field fn id read outside the impl") {
    assert!(invalid(through_object()));
    assert!(invalid(through_param()));
}
