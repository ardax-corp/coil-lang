// `2 as float` converts the literal: a small int literal is already its
// byte or int, but a float or bool cast changes its word. Inlining `mix`
// substitutes the literal argument under its casts.

fn mix(int n) -> float {
    let f = n as float;
    let back = (f * 2.5) as int;
    let flag = back as bool;
    return f + (flag as int) as float;
}

test("literal casts to float and bool") {
    let f = 2 as float;
    assert(f * 2.5 == 5.0)?;
    assert((0 as bool) == false)?;
    assert((7 as bool) == true)?;
    assert(((2 as float) as int) == 2)?;
}

test("literal casts to byte and int keep the value") {
    let b = 200 as byte;
    assert((b as int) == 200)?;
    assert(((65 as byte) as int) == 65)?;
}

test("an inlined callee casts its literal argument") {
    assert(mix(2) == 3.0)?;
    assert(mix(0) == 0.0)?;
}
