// #1208: a type alias imported with `use` is the aliased type, as the
// qualified `qpath_provider::Meters` is (`type Meters = int;`).
use tests::assert;
use qpath_provider::Meters;

fn double(Meters m) -> Meters {
    return m * 2;
}

test("an imported alias is its aliased type") {
    let m: Meters = 12;
    let n = m + 1;
    assert!(n == 13);
}

test("an imported alias in a signature") {
    let i: int = double(5);
    assert!(i == 10);
}

test("the imported and qualified spellings agree") {
    let a: Meters = 3;
    let b: qpath_provider::Meters = a;
    assert!(b == 3);
}
