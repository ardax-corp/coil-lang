// Exercised by `coil test` — success-path assert / Result edges.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

test("assert arithmetic") {
    assert(true)?;
    assert(1 + 1 == 2, "arithmetic")?;
}

test("match on assert Ok") {
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match assert(true) {
                    Result::Ok(_) => "ok",
                    Result::Err(_) => "bad",
                },
            ),
        ),
    );
}
