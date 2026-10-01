// Comparison and logical operators.
test("int equality") {
    assert(1 == 1)?;
    assert(1 != 2)?;
    assert(!(1 == 2))?;
}

test("int ordering") {
    assert(1 < 2)?;
    assert(2 > 1)?;
    assert(1 <= 1)?;
    assert(2 >= 2)?;
    assert(3 >= 2)?;
    assert(!(5 < 3))?;
    assert(-5 < 0)?;
    assert(-1 <= -1)?;
    assert(!(-5 > 0))?;
}

test("float comparisons") {
    assert(1.0 < 2.0)?;
    assert(2.5 >= 2.5)?;
    assert(3.0 != 3.1)?;
}

test("bool logical and or") {
    assert(true && true)?;
    assert(!(true && false))?;
    assert(true || false)?;
    assert(!(false || false))?;
}

test("logical not") {
    assert(!false)?;
    assert(!(!true))?;
    assert(!0)?;
    assert(!(!1))?;
}

test("string equality") {
    assert("hi" == "hi")?;
    assert("a" != "b")?;
}

test("comparison of computed values") {
    let x = 10;
    let y = 3;
    assert(x + y == 13)?;
    assert(x > y)?;
    assert(x % y == 1)?;
}

// The literal cases above fold at compile time (`assert(1 < 2)` compiles to
// a constant). `opaque` hides a value from the optimizer (it round-trips
// through a `Vec`), so the cases below run the VM's comparison instructions.
fn opaque(int x) -> int {
    let v: Vec<int> = Vec::new();
    v.push(x);
    return v[0];
}

fn opaque_f(float x) -> float {
    let v: Vec<float> = Vec::new();
    v.push(x);
    return v[0];
}

fn opaque_s(string x) -> string {
    let v: Vec<string> = Vec::new();
    v.push(x);
    return v[0];
}

test("runtime int comparisons") {
    assert(opaque(1) == opaque(1))?;
    assert(opaque(1) != opaque(2))?;
    assert(opaque(1) < opaque(2))?;
    assert(!(opaque(2) < opaque(2)))?;
    assert(opaque(2) <= opaque(2))?;
    assert(opaque(3) > opaque(-3))?;
    assert(opaque(-3) >= opaque(-3))?;
    assert(!(opaque(-5) > opaque(0)))?;
}

test("runtime float comparisons") {
    assert(opaque_f(1.0) < opaque_f(2.0))?;
    assert(opaque_f(2.5) >= opaque_f(2.5))?;
    assert(opaque_f(3.0) != opaque_f(3.1))?;
    assert(!(opaque_f(-0.5) > opaque_f(0.0)))?;
}

test("runtime string equality") {
    assert(opaque_s("hi") == opaque_s("h" + "i"))?;
    assert(opaque_s("a") != opaque_s("b"))?;
}

test("runtime logical operators") {
    let t = opaque(1) == opaque(1);
    let f = opaque(1) == opaque(2);
    assert(t && !f)?;
    assert(!(t && f))?;
    assert(t || f)?;
    assert(!(f || f))?;
}
