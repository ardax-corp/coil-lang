use fn_macros::{square, check, sum, quad, describe, counter, swap};

class Clicks {
    pub n: int,
}

impl Show for Clicks {
    fn show(Clicks __show_Clicks) -> string {
        return "Clicks";
    }
}

impl String for Clicks {
    fn to_string(Clicks __str_Clicks) -> string {
        return "Clicks";
    }
}

impl Clicks {
    pub fn bump() -> int {
        self.n += 1;
        return self.n;
    }
}

fn id(int x) -> int {
    return x;
}

test("expression position keeps each argument whole") {
    assert(3 * 3 == 9)?;
    assert((1 + 2) * (1 + 2) == 9)?;
    assert(id(4) * id(4) - 1 == 15)?;
}

test("a last Vec<Expr> parameter takes the rest") {
    assert(0 == 0)?;
    assert(7 == 7)?;
    assert(1 + 2 + (3 * 4) == 15)?;
}

test("macros in a macro's output expand next round") {
    assert((2 * 2) * (2 * 2) == 16)?;
    assert((3 * 3) * (3 * 3) == 81)?;
}

test("arguments arrive with their source text and kind") {
    assert("call: id(1)" == "call: id(1)")?;
    assert("ident: x" == "ident: x")?;
    assert("path: a.b" == "path: a.b")?;
    assert("other: 1 + 2" == "other: 1 + 2")?;
    assert("literal: \"hi\"" == "literal: \"hi\"")?;
    assert("literal: -3" == "literal: -3")?;
}

test("statement position: the output is statements") {
    if !(2 * 2 == 4) {
        panic "check failed: " + "square!(2) == 4";
    }
    let a = 1;
    let b = 2;
    let tmp__m = a;
    a = b;
    b = tmp__m;
    assert(a == 2 && b == 1)?;
    let tmp = 100;
    let tmp__m = a;
    a = b;
    b = tmp__m;
    let tmp__m = a;
    a = b;
    b = tmp__m;
    assert(a == 2 && b == 1 && tmp == 100)?;
}

test("item position: the output is declarations") {
    let c = new Clicks(0);
    c.bump();
    assert(c.bump() == 2)?;
}
