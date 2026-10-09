// Unary `-` on a `T: Num` value in a shared generic body goes through the
// `Neg` dictionary (#803); a free fn named like an operator trait's method
// is still that fn under a `T: Num` bound.

fn neg<T: Num>(T a) -> T {
    return -a;
}

fn twice<T: Num>(T a) -> T {
    return neg(a) - a;
}

fn add<T: Num>(T a) -> T {
    return a + a + a;
}

fn thrice_less_one<T: Num>(T a) -> T {
    return add(a) - a;
}

fn neg_bound<T: Neg>(T a) -> T {
    return -a;
}

test("int through a forwarded dictionary") {
    assert(twice(2) == -4)?;
    assert(neg(5) == -5)?;
}

test("float through a forwarded dictionary") {
    let y = twice(1.5);
    assert(y == -3.0)?;
    assert(neg(-0.5) == 0.5)?;
}

test("a Neg bound alone") {
    assert(neg_bound(3) == -3)?;
    assert(neg_bound(2.0) == -2.0)?;
}

test("a free fn named add is not Add::add") {
    assert(thrice_less_one(2) == 4)?;
    assert(thrice_less_one(1.0) == 2.0)?;
}
