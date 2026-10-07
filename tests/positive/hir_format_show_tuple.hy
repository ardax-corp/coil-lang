// `%v` of a tuple or record shows it field by field through temps; the
// `format` call stages its arguments so the temps have no operand below.
use string::format;

test("tuple and record literals") {
    assert(format("%v", (1, 2)) == "(1, 2)")?;
    assert(format("%v", { a: 3, b: 4 }) == "{ a: 3, b: 4 }")?;
}

test("locals among other arguments") {
    let t = (7, "x");
    let n = 5;
    let s = format("%i %v %s", n, t, "end");
    assert(s == "5 (7, x) end")?;
}

test("nested tuple") {
    let t = ((1, 2), 3);
    assert(format("%v", t) == "((1, 2), 3)")?;
}

test("format result passed on") {
    let parts = Vec::from([format("%v", (1, true))]);
    assert(parts[0] == "(1, true)")?;
}
