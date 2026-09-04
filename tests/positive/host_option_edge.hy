// Host-edge Option pack: Vec pop/remove and gc get/unroot/upgrade.
use gc::{get, root, unroot, upgrade, weak};

test("vec pop and remove string niche") {
    let v = Vec::from(["a", "b", "c"]);
    let last = match v.pop() {
        Option::Some(value) => value,
        Option::None => "none",
    };
    assert(last == "c")?;
    let mid = match v.remove(0) {
        Option::Some(value) => value,
        Option::None => "none",
    };
    assert(mid == "a")?;
    let miss = match v.remove(9) {
        Option::Some(_) => false,
        Option::None => true,
    };
    assert(miss)?;
}

test("gc get unroot upgrade string") {
    let r = root("pin");
    let got = match get(r) {
        Option::Some(value) => value,
        Option::None => "none",
    };
    assert(got == "pin")?;
    let taken = match unroot(r) {
        Option::Some(value) => value,
        Option::None => "none",
    };
    assert(taken == "pin")?;
    let empty = match get(r) {
        Option::Some(_) => false,
        Option::None => true,
    };
    assert(empty)?;

    let w = weak("ephem");
    let up = match upgrade(w) {
        Option::Some(value) => value,
        Option::None => "none",
    };
    assert(up == "ephem")?;
}
