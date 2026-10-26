// #2039: a recursive enum keeps its self-referencing payload types when its
// module is imported, so the module's own recursive calls still check.
use tests::assert;
use recursive_enum_provider::{Expr, eval, Chain, chain_len, Outer, Inner, depth};

test("an imported recursive enum evaluates through its module's function") {
    assert!(eval(Expr::Add(Expr::Num(1), Expr::Num(2))) == 3);
    assert!(eval(Expr::Neg { inner: Expr::Add(Expr::Num(4), Expr::Num(1)) }) == -5);
}

test("an imported generic enum that names itself in a payload") {
    assert!(chain_len(Chain::Link(1, Chain::Link(2, Chain::End))) == 2);
}

test("an imported enum whose payload names an enum declared after it") {
    assert!(depth(Outer::Via(Inner::Back(Outer::Via(Inner::Back(Outer::Leaf))))) == 2);
}
