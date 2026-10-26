// #2039: a recursive enum keeps its self-referencing payload types when its
// module is imported, so the module's own recursive calls still check.
use tests::assert;
use recursive_enum_provider::{Expr, eval};

test("an imported recursive enum evaluates through its module's function") {
    assert!(eval(Expr::Add(Expr::Num(1), Expr::Num(2))) == 3);
    assert!(eval(Expr::Neg { inner: Expr::Add(Expr::Num(4), Expr::Num(1)) }) == -5);
}
