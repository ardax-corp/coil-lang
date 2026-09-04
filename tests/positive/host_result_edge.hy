// Host-edge Result pack: Result<(), E> Option-shaped + heap-heap Result.
use io::{result_unit_probe};
use string::{from_bytes, to_bytes};

test("unit result probe ok and err") {
    match result_unit_probe(0) {
        Result::Ok(_) => {},
        Result::Err(_) => {
            assert(false)?;
        },
    };
    match result_unit_probe(-1) {
        Result::Ok(_) => {
            assert(false)?;
        },
        Result::Err(_) => {},
    };
}

test("from_bytes heap-heap result") {
    match from_bytes(to_bytes("hi")) {
        Result::Ok(s) => {
            assert(s == "hi")?;
        },
        Result::Err(_) => {
            assert(false)?;
        },
    };
}
