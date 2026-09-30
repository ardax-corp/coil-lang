// A user type whose name only differs from a built-in spelling by case
// (`Unit` vs the unit type) is a nominal instance head, so its derived and
// hand-written trait instances are the ones `==` / method calls reach (#546).
// `void` stays the unit type's spelling in instance heads.

#[derive(Eq, Show)]
class Unit {}

trait Tag<T> {
    fn tag(T x) -> int {}
}

impl Tag for Unit {
    pub fn tag(Unit u) -> int {
        return 7;
    }
}

impl Tag for void {
    pub fn tag(() u) -> int {
        return 1;
    }
}

test("derived == on a field-less class named Unit") {
    assert(new Unit() == new Unit())?;
    assert(!(new Unit() != new Unit()))?;
    let a = new Unit();
    let b = new Unit();
    assert(a == b)?;
    assert(a.eq(b))?;
}

test("derived Show on Unit") {
    assert(new Unit().show() == "Unit {  }")?;
}

test("hand-written instance on Unit is distinct from the unit type's") {
    assert(new Unit().tag() == 7)?;
    assert(().tag() == 1)?;
}
