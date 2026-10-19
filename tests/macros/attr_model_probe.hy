// An attribute macro on a function sees its parameters, return type, type
// parameters, `pub` / `static` / `gen`, the owning class of a method, and its
// body as written; `string`, `int` and `bool` arguments bind by name.
use tests::assert;
use macro_probe::{fn_info, body_text, flags};

#[fn_info]
fn add(int a, string b) -> int {
    return a + b.len();
}

#[fn_info]
fn first<T>(Vec<T> xs) -> Option<T> {
    if xs.len() == 0 {
        return Option::None;
    }
    return Option::Some(xs[0]);
}

#[fn_info]
gen fn ticks() -> int {
    yield 1;
}

class Meter {
    pub n: int,
}

impl Meter {
    #[fn_info]
    pub fn read(int scale) -> int {
        return self.n * scale;
    }

    #[fn_info]
    pub static fn zero() -> Meter {
        return new Meter(0);
    }
}

#[body_text]
fn noisy() -> int {
    let x = 1;
    return x + 1;
}

#[flags(label = "hi", times = 3, loud = false)]
fn by_name() -> int {
    return 1;
}

#[flags(loud = false, times = 1, label = "yo")]
fn in_order() -> int {
    return 2;
}

test("parameters, return type and flags of a free function") {
    assert!(
        add_info() == "add(int a, string b) -> int tparams= pub=n static=n gen=n owner= args=a, b",
    );
    assert!(add(1, "xy") == 3);
}

test("a generic function's type parameters") {
    assert!(
        first_info() ==
        "first(Vec<T> xs) -> Option<T> tparams=T pub=n static=n gen=n owner= args=xs",
    );
    assert!(first(Vec::from([4, 5])) == Option::Some(4));
}

test("a gen fn is marked") {
    let s = ticks_info();
    assert!(s == "ticks() -> int tparams= pub=n static=n gen=y owner= args=", s);
}

test("a method knows its class; a static method is marked") {
    assert!(
        Meter::read_info() ==
        "read(int scale) -> int tparams= pub=y static=n gen=n owner=Meter args=scale",
    );
    assert!(Meter::zero_info() == "zero() -> Meter tparams= pub=y static=y gen=n owner=Meter args=");
    assert!(new Meter(2).read(5) == 10);
    assert!(Meter::zero().n == 0);
}

test("the body as written") {
    let b = noisy_body();
    assert!(noisy() == 2);
    // Line endings are the checkout's own (CRLF on a Windows checkout).
    let lf = b == "{let x = 1;\n    return x + 1;\n}";
    let crlf = b == "{let x = 1;\r\n    return x + 1;\r\n}";
    assert!(lf || crlf, b);
}

test("string, int and bool arguments by name, in any order") {
    assert!(by_name_flags() == "hi,hi,hi");
    assert!(in_order_flags() == "yo");
    assert!(by_name() + in_order() == 3);
}
