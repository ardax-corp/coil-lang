// A stack array of boxed enum values, and a variant whose payload is a
// two-word Option (stored boxed), lower straight to MIR.
enum Op {
    Add(int, int),
    Neg(Option<int>),
    Nop,
}
fn ev(Op op) -> int {
    return match op { Op::Add(a, b) => a + b, Op::Neg(Option::Some(n)) => n, default => 1 };
}
fn mk() -> Op {
    return Op::Neg(Option::Some(6));
}
fn arr() -> int {
    let ops = [Op::Add(2, 3), Op::Nop];
    let acc = 0;
    for op in ops { acc = acc + ev(op); }
    return acc;
}
test("stack array of enums and a boxed Option payload") {
    assert(ev(mk()) + arr() == 12)?;
}
