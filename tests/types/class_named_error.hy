// The FFI `Error` is not reserved: a user `class Error` or `enum Error`, in
// any letter case and in any module, is its own type (#1701).
use tests::assert;
use error_class_provider::{fail, code_of};

class ERROR {
    pub msg: string,
}

enum Error {
    Fatal,
    Soft(int),
}

fn text(ERROR e) -> string {
    return e.msg;
}

fn weight(Error e) -> int {
    return match e {
        Error::Fatal => 100,
        Error::Soft(n) => n,
    };
}

test("a class named Error in another module") {
    assert!(code_of(fail(7)) == 7);
}

test("a class and an enum spelled like Error") {
    assert!(text(new ERROR("boom")) == "boom");
    assert!(weight(Error::Soft(3)) == 3);
    assert!(weight(Error::Fatal) == 100);
}
