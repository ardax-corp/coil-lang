// A derive sees a type as written: its kind, generics, fields (name, type,
// `pub`, owned field attributes), variants (shape, arity, discriminant), the
// scalar `repr` and the type's other attributes (but not an attribute macro's,
// which has already run).
use tests::assert;
use macro_probe::{Inspect, tagged};

#[derive(Inspect)]
class Point {
    pub x: int,
    #[probe(label = "why")]
    y: Vec<string>,
}

#[derive(Inspect)]
class Pair<A, B> {
    pub first: A,
    pub second: Option<B>,
}

#[derive(Inspect)]
enum Shape {
    Dot,
    #[probe(label = "round")]
    Circle(float),
    Rect { w: float, h: float },
    Pair(int, int),
}

#[repr(int)]
#[derive(Inspect)]
enum Level {
    Low = 1,
    High = 5,
}

#[tagged]
#[derive(Inspect)]
class Marked {
    pub n: int,
}

test("a class: fields with types, visibility and field attributes") {
    assert!(
        inspect_Point() == "class Point class=y enum=n generics= repr= tagged=n has_repr=n self=Point | x:int head=int args=0 pub=y label=- | y:Vec<string> head=Vec args=1 pub=n label=why",
    );
}

test("a generic class: parameters and self_type") {
    assert!(
        inspect_Pair() == "class Pair class=y enum=n generics=<A><B> repr= tagged=n has_repr=n self=Pair<A, B> | first:A head=A args=0 pub=y label=- | second:Option<B> head=Option args=1 pub=y label=-",
    );
}

test("an enum: each variant's shape and arity") {
    assert!(
        inspect_Shape() == "enum Shape class=n enum=y generics= repr= tagged=n has_repr=n self=Shape | Dot unit/0 label=- | Circle tuple/1 label=round | Rect record/2 label=- | Pair tuple/2 label=-",
    );
}

test("a scalar enum: repr and discriminants as written") {
    assert!(
        inspect_Level() == "enum Level class=n enum=y generics= repr=int tagged=n has_repr=y self=Level | Low unit/0=1 label=- | High unit/0=5 label=-",
    );
}

test("an attribute macro's own attribute is gone when the derive runs") {
    let m = new Marked(3);
    let s = inspect_Marked();
    assert!(m.n == 3);
    assert!(
        s == "class Marked class=y enum=n generics= repr= tagged=n has_repr=n self=Marked | n:int head=int args=0 pub=y label=-",
        s,
    );
}
