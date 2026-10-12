// #915 follow-ups: a mono clone calls a ground trait method under a pending
// operand (`u.label() + ":"`), and a shared body (a function argument keeps
// it unspecialized) returns its type parameter inside an `Option`.
trait Label<T> {
    fn label(T x) -> string {}
}

class Crate {
    pub kg: int,
}

impl Label for Crate {
    pub fn label(Crate c) -> string {
        return "crate";
    }
}

impl Label for int {
    pub fn label(int x) -> string {
        return "int";
    }
}

fn tag<U: Label>(U u) -> string {
    return u.label() + ":";
}

fn tag_after<T, U: Label>(T x, U u) -> string {
    return u.label() + ":";
}

fn wrap_result<T>(unit -> T f) -> Option<T> {
    return Option::Some(f());
}

fn make_crate() -> Crate {
    return new Crate(9);
}

fn make_float() -> float {
    return 2.5;
}

test("a clone's ground trait call under a pending operand") {
    assert(tag(new Crate(1)) == "crate:")?;
    assert(tag(4) == "int:")?;
    assert(tag_after(4, new Crate(1)) == "crate:")?;
}

test("a shared body returns its type parameter inside an Option") {
    let c = match wrap_result(make_crate) {
        Option::Some(v) => v.kg,
        Option::None => -1,
    };
    assert(c == 9)?;
    let f = match wrap_result(make_float) {
        Option::Some(v) => v,
        Option::None => -1.0,
    };
    assert(f == 2.5)?;
}
