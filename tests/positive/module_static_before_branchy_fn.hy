// #958: a module's statics keep their values when the module's last function
// has branches (the static-init setup is laid out after that function and
// must still call each initializer). Failed at -O0 and under `--coverage`.
use tests::assert;
use static_then_branchy_fn::{RATIO, COUNT, NAME, magnitude};

test("float const static is initialized") {
    assert!(RATIO > 3.14);
    assert!(RATIO < 3.15);
}

test("int and string statics are initialized") {
    assert!(COUNT == 41);
    assert!(NAME == "static");
}

test("the function after the statics still runs") {
    assert!(magnitude(0 - 3) == 3);
    assert!(magnitude(COUNT) == 41);
}
