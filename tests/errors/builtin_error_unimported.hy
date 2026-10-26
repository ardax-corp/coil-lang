// A test body that `?`s a `TtyError` / `StringError` it never imports
// still lowers: the builtin error enums keep their tags across modules.
use tests::assert;
use builtin_error_provider::{tty_ok, string_ok};

test("TtyError through an unimported module") {
    let x = tty_ok()?;
    assert!(x == 1);
}

test("StringError through an unimported module") {
    let x = string_ok()?;
    assert!(x == 2);
}
