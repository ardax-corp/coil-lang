use tests::assert;
use fn_macros::{square, check, sum, quad, describe, counter, swap, ensure, count};

class Clicks {
    pub n: int,
}

impl Show for Clicks {
    fn show(Clicks __show_Clicks) -> string {
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
    if true != (3 * 3 == 9) {
        raise "Assertion: 'square!(3) == 9' failed";
    }
    if true != ((1 + 2) * (1 + 2) == 9) {
        raise "Assertion: 'square!(1 + 2) == 9' failed";
    }
    if true != (id(4) * id(4) - 1 == 15) {
        raise "Assertion: 'square!(id(4)) - 1 == 15' failed";
    }
}

test("a last Vec<Expr> parameter takes the rest") {
    if true != (0 == 0) {
        raise "Assertion: 'sum!() == 0' failed";
    }
    if true != (7 == 7) {
        raise "Assertion: 'sum!(7) == 7' failed";
    }
    if true != (1 + 2 + (3 * 4) == 15) {
        raise "Assertion: 'sum!(1, 2, 3 * 4) == 15' failed";
    }
}

test("macros in a macro's output expand next round") {
    if true != ((2 * 2) * (2 * 2) == 16) {
        raise "Assertion: 'quad!(2) == 16' failed";
    }
    if true != ((3 * 3) * (3 * 3) == 81) {
        raise "Assertion: 'square!(square!(3)) == 81' failed";
    }
}

test("arguments arrive with their source text and kind") {
    if true != ("call: id(1)" == "call: id(1)") {
        raise "Assertion: 'describe!(id(1)) == \"call: id(1)\"' failed";
    }
    if true != ("ident: x" == "ident: x") {
        raise "Assertion: 'describe!(x) == \"ident: x\"' failed";
    }
    if true != ("path: a.b" == "path: a.b") {
        raise "Assertion: 'describe!(a.b) == \"path: a.b\"' failed";
    }
    if true != ("other: 1 + 2" == "other: 1 + 2") {
        raise "Assertion: 'describe!(1 + 2) == \"other: 1 + 2\"' failed";
    }
    if true != ("literal: \"hi\"" == "literal: \"hi\"") {
        raise "Assertion: 'describe!(\"hi\") == \"literal: \\\"hi\\\"\"' failed";
    }
    if true != ("literal: -3" == "literal: -3") {
        raise "Assertion: 'describe!(-3) == \"literal: -3\"' failed";
    }
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
    if true != (a == 2 && b == 1) {
        raise "Assertion: 'a == 2 && b == 1' failed";
    }
    let tmp = 100;
    let tmp__m = a;
    a = b;
    b = tmp__m;
    let tmp__m = a;
    a = b;
    b = tmp__m;
    if true != (a == 2 && b == 1 && tmp == 100) {
        raise "Assertion: 'a == 2 && b == 1 && tmp == 100' failed";
    }
}

test("item position: the output is declarations") {
    let c = new Clicks(0);
    c.bump();
    if true != (c.bump() == 2) {
        raise "Assertion: 'c.bump() == 2' failed";
    }
}

test("a macro overloads by arity") {
    if true != (fn_macros::ensure_result(1 + 1 == 2, "ensure failed: 1 + 1 == 2") == Result::Ok(())) {
        raise "Assertion: 'ensure!(1 + 1 == 2) == Result::Ok(())' failed";
    }
    if true != (fn_macros::ensure_result(1 > 2, "ensure failed: 1 > 2") ==
                Result::Err("ensure failed: 1 > 2")) {
        raise "Assertion: 'ensure!(1 > 2) == Result::Err(\"ensure failed: 1 > 2\")' failed";
    }
    if true != (fn_macros::ensure_result(1 > 2, "custom") == Result::Err("custom")) {
        raise "Assertion: 'ensure!(1 > 2, \"custom\") == Result::Err(\"custom\")' failed";
    }
    if true != (0 == 0) {
        raise "Assertion: 'count!() == 0' failed";
    }
    if true != (1 == 1) {
        raise "Assertion: 'count!(a) == 1' failed";
    }
    if true != (3 == 3) {
        raise "Assertion: 'count!(a, b + c, d) == 3' failed";
    }
}
