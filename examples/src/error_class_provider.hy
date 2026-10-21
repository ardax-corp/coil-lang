// Helper module for tests/types/class_named_error.hy (#1701): a class named
// like the FFI `Error`, outside the entry module.

class Error {
    pub code: int,
}

fn fail(int code) -> Error {
    return new Error(code);
}

fn code_of(Error e) -> int {
    return e.code;
}
