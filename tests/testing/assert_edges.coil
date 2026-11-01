// Exercised by `coil test` — success-path assert / Result edges.
use tests::assert;
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

test("assert arithmetic") {
    assert!(true);
    assert!(1 + 1 == 2, "arithmetic");
}

fn passes() -> Result<(), string> {
    assert!(true);
    return Result::Ok(());
}

test("match on assert Ok") {
    let _ = write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match passes() {
                    Result::Ok(_) => "ok",
                    Result::Err(_) => "bad",
                },
            ),
        ),
    );
}
