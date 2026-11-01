// `#[derive(Default)]`: `Ty::default()` gives each class field its type's
// default and an enum its first case with defaulted payloads.
use tests::assert;
use string::format;

#[derive(Default, Show)]
class Conf {
    pub n: int,
    pub name: string,
    pub on: bool,
    pub f: float,
}

#[derive(Default)]
enum Mode {
    First(int, string),
    Second,
}

test("class fields take their type defaults") {
    let c = Conf::default();
    assert!(c.n == 0);
    assert!(c.name == "");
    assert!(!c.on);
    assert!(c.f == 0.0);
    assert!(format("%v", c) == "Conf { n: 0, name: , on: false, f: 0.0 }", format("%v", c));
}

test("defaults are fresh, independent objects") {
    let a = Conf::default();
    let b = Conf::default();
    a.n = 5;
    assert!(b.n == 0);
}

test("enum default is the first case") {
    let m = Mode::default();
    let ok = match m {
        Mode::First(v, s) => v == 0 && s == "",
        Mode::Second => false,
    };
    assert!(ok);
}
