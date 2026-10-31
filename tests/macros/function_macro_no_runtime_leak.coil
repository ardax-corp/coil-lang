// A macro provider's macros and the `macro` module it uses run at compile
// time only: user classes may share their names (`Code`, `Reader`).
use tests::assert;
use fn_macros::square;

class Code {
    pub n: int,
}

class Reader {
    pub s: string,
}

test("user classes named like the macro model") {
    let c = new Code(square!(3));
    let r = new Reader("x");
    assert!(c.n == 9 && r.s == "x");
}
