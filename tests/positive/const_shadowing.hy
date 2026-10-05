// A parameter or `let` named like a module `const` reads the local, not
// the const, for the rest of its scope.
const LIMIT = 10;

fn shadowed(int LIMIT) -> int {
    return LIMIT + 1;
}

fn local_shadow() -> int {
    let LIMIT = 3;
    return LIMIT * 2;
}

fn plain() -> int {
    return LIMIT;
}

fn inner_const() -> int {
    const K = 4;
    return K + LIMIT;
}

fn lambda_param() -> int {
    let f = fn (int LIMIT) => LIMIT * 3;
    return f(5) + LIMIT;
}

test("parameter shadows const") {
    assert(shadowed(41) == 42)?;
}

test("let shadows const") {
    assert(local_shadow() == 6)?;
}

test("const read") {
    assert(plain() == 10)?;
}

test("inner const") {
    assert(inner_const() == 14)?;
}

test("lambda parameter does not leak") {
    assert(lambda_param() == 25)?;
}
