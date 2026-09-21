// COI-404: cross-module Result<Path, IoError> match must take Ok.
// Caller does not import IoError — that was enough to lose niche layout.
use coi404_path::{Path};

test("cross-module Path IoError instance clone is Ok") {
    let a = Path::from("a");
    let j = match a.clone_ok() {
        Result::Ok(p) => p,
        Result::Err(_) => panic "clone",
    };
    assert(j.as_str() == "a")?;
}

test("cross-module Path IoError join empty is Ok") {
    let a = Path::from("a");
    let b = Path::from("");
    let j = match a.join_empty(b) {
        Result::Ok(p) => p,
        Result::Err(_) => panic "join empty",
    };
    assert(j.as_str() == "a")?;
}
