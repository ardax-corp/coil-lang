// String concatenation pushes the `%s%s` format string before its operands.
// An operand whose code STOREs (an inlined call's parameter temp, a match, a
// constructor) must be staged first: locals and the operand stack share
// memory, so the store would overwrite the format string.
use string::format;

fn get(int k) -> string {
    return "a";
}

fn mk(int k) -> fn(int x) -> string {
    return fn (int y) => "c";
}

class P {
    pub x: int,
}

impl P {
    pub fn tag(int k) -> string {
        return "p";
    }
}

test("inlined call operand next to an indirect call") {
    let g = mk(3);
    let s = get(1) + g(2);
    assert(s == "ac")?;
    let p = new P(1);
    assert(p.tag(1) + g(2) == "pc")?;
}

test("compound assign with a call rhs") {
    let g = mk(3);
    let s = g(2);
    s += get(1);
    s += get(2) + g(1);
    assert(s == "caac")?;
}

test("format with call args") {
    let g = mk(3);
    assert(format("%s-%s", get(1), g(2)) == "a-c")?;
}
