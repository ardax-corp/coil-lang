// A shared generic body's bound operators and trait methods go through
// its own dictionary parameters on direct MIR: each operand's word, the
// dictionary, then the code pointer in the method's slot. Its open
// parameters come boxed and unbox at entry; a result typed `T` unboxes.
// (Only open parameters: a `PolyFn` call boxes a concrete one, #1089.)
use tests::assert;

trait Label<T> {
    fn label(T self) -> string {}
}

impl Label for int {
    fn label(int self) -> string {
        return "i" + self.show();
    }
}

impl Label for string {
    fn label(string self) -> string {
        return "s" + self;
    }
}

trait Twice<T> {
    fn twice(T self) -> T {}
}

impl Twice for int {
    fn twice(int self) -> int {
        return self * 2;
    }
}

impl Twice for string {
    fn twice(string self) -> string {
        return self + self;
    }
}

fn sum_sq<T: Num>(T a, T b) -> T {
    let acc = a * b;
    let i = 0;
    while i < 2 {
        acc = acc + a * b - b;
        i += 1;
    }
    return acc;
}

fn labels<T: Label>(T x) -> string {
    let out = "";
    let i = 0;
    while i < 2 {
        // Each call allocates: the open word stays live across it.
        out = out + x.label() + ",";
        i += 1;
    }
    return out;
}

fn bigger<T: Ord>(T a, T b) -> T {
    if a > b {
        return a;
    }
    return b;
}

fn quad<T: Twice>(T x) -> T {
    let out = x;
    let i = 0;
    while i < 2 {
        out = out.twice().twice();
        i += 1;
    }
    return out;
}

fn flip<T: Neg>(T a) -> T {
    return -(-(-a));
}

test("bound operators through the dictionary") {
    let f = sum_sq;
    assert!(f(3, 2) == 6 + 4 + 4);
    assert!(f(1.5, 2.0) == 3.0 + 1.0 + 1.0);
    let g = flip;
    assert!(g(4) == -4);
    assert!(g(2.5) == -2.5);
}

test("bound methods keep heap words alive") {
    let l = labels;
    assert!(l(7) == "i7,i7,");
    assert!(l("ab") == "sab,sab,");
}

test("an open result unboxes") {
    let b = bigger;
    assert!(b(3, 9) == 9);
    assert!(b(2.5, -1.0) == 2.5);
    let q = quad;
    assert!(q(3) == 48);
    assert!(q("a") == "aaaaaaaaaaaaaaaa");
}
