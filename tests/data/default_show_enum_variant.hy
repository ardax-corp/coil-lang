// A non-generic enum with no `Show` derive or impl shows as its qualified
// variant name (`E::V`, payload elided), through `%v` and `show` (#1749).
use tests::assert;
use string::format;

enum Errr {
    Fatal,
    Soft(int),
    Rec { code: int, why: string },
}

fn describe(Errr e) -> string {
    return format("%v", e);
}

test("unit variant shows its name") {
    assert!(format("%v", Errr::Fatal) == "Errr::Fatal", format("%v", Errr::Fatal));
    assert!(show(Errr::Fatal) == "Errr::Fatal");
}

test("payload variants show their name without the payload") {
    assert!(format("%v", Errr::Soft(3)) == "Errr::Soft", format("%v", Errr::Soft(3)));
    let r = Errr::Rec { code: 1, why: "x" };
    assert!(r.show() == "Errr::Rec", r.show());
}

test("through a function parameter and a tuple") {
    assert!(describe(Errr::Fatal) == "Errr::Fatal", describe(Errr::Fatal));
    let s = format("%v", (Errr::Soft(2), 1));
    assert!(s == "(Errr::Soft, 1)", s);
}
