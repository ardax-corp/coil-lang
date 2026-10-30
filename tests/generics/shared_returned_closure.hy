// A closure a generic function's shared body returns (past the clone
// budget, E0804) is called at the call's ground type: its type parameter
// arguments boxed and its type parameter result unboxed on the way, a
// niche `Option` included, through a function parameter, a `let` and in
// place (#1532).
// @flags --clone-budget 8
use tests::assert;
use string::format;

class A {
    pub v: int,
}

class B {
    pub v: int,
}

class C {
    pub v: int,
}

class D {
    pub v: int,
}

class E {
    pub v: int,
}

class F {
    pub v: int,
}

class G {
    pub v: int,
}

class H {
    pub v: int,
}

class Cell {
    pub v: int,
}

fn konst<T>(T x) -> T -> T {
    return fn (T v) use (x) => x;
}

fn ident<T>(T x) -> T -> T {
    return fn (T v) => v;
}

fn maker<T>(T x) -> unit -> T {
    return fn () use (x) => x;
}

fn scaled<T>(T x, int n) -> T -> int {
    let m = n * 2;
    return fn (T v) use (m) => m;
}

fn shown<T: Show>(T x) -> T -> string {
    return fn (T v) use (x) => format("%v/%v", x, v);
}

fn app<V>(V -> V f, V v) -> V {
    return f(v);
}

fn relay<U>(U u) -> U {
    return app(ident(u), u);
}

fn ground_in<U>(U u) -> int {
    let k = konst(3);
    return k(4);
}

impl Show for A {
    pub fn show(A self) -> string {
        return "A";
    }
}

impl Show for B {
    pub fn show(B self) -> string {
        return "B";
    }
}

impl Show for C {
    pub fn show(C self) -> string {
        return "C";
    }
}

impl Show for D {
    pub fn show(D self) -> string {
        return "D";
    }
}

impl Show for E {
    pub fn show(E self) -> string {
        return "E";
    }
}

impl Show for F {
    pub fn show(F self) -> string {
        return "F";
    }
}

impl Show for G {
    pub fn show(G self) -> string {
        return "G";
    }
}

impl Show for H {
    pub fn show(H self) -> string {
        return "H";
    }
}

fn at_k(int -> int f) -> int {
    return f(6);
}

fn at_s(string -> string f) -> string {
    return f("arg");
}

fn at_f(float -> float f) -> float {
    return f(0.5);
}

fn at_b(bool -> bool f) -> bool {
    return f(false);
}

fn at_c(Cell -> Cell f) -> int {
    return f(new Cell(0)).v;
}

fn at_o(unit -> Option<string> f) -> string {
    return match f() {
        Some(v) => v,
        None => "none",
    };
}

fn at_m(unit -> int f) -> int {
    return f();
}

fn at_sc(string -> int f) -> int {
    return f("t");
}

fn fill() {
    let a = konst(new A(1));
    let b = konst(new B(1));
    let c = konst(new C(1));
    let d = konst(new D(1));
    let e = konst(new E(1));
    let f = konst(new F(1));
    let g = konst(new G(1));
    let h = konst(new H(1));
    let ia = ident(new A(1));
    let ib = ident(new B(1));
    let ic = ident(new C(1));
    let id = ident(new D(1));
    let ie = ident(new E(1));
    let i_f = ident(new F(1));
    let ig = ident(new G(1));
    let ih = ident(new H(1));
    let ma = maker(new A(1));
    let mb = maker(new B(1));
    let mc = maker(new C(1));
    let md = maker(new D(1));
    let me = maker(new E(1));
    let mf = maker(new F(1));
    let mg = maker(new G(1));
    let mh = maker(new H(1));
    let sa = scaled(new A(1), 1);
    let sb = scaled(new B(1), 1);
    let sc = scaled(new C(1), 1);
    let sd = scaled(new D(1), 1);
    let se = scaled(new E(1), 1);
    let sf = scaled(new F(1), 1);
    let sg = scaled(new G(1), 1);
    let sh = scaled(new H(1), 1);
    let wa = shown(new A(1));
    let wb = shown(new B(1));
    let wc = shown(new C(1));
    let wd = shown(new D(1));
    let we = shown(new E(1));
    let wf = shown(new F(1));
    let wg = shown(new G(1));
    let wh = shown(new H(1));
    let ra = relay(new A(1));
    let rb = relay(new B(1));
    let rc = relay(new C(1));
    let rd = relay(new D(1));
    let re = relay(new E(1));
    let rf = relay(new F(1));
    let rg = relay(new G(1));
    let rh = relay(new H(1));
    let ga = ground_in(new A(1));
    let gb = ground_in(new B(1));
    let gc = ground_in(new C(1));
    let gd = ground_in(new D(1));
    let ge = ground_in(new E(1));
    let gf = ground_in(new F(1));
    let gg = ground_in(new G(1));
    let gh = ground_in(new H(1));
}

test("a shared body's closure through a function parameter") {
    fill();
    assert!(at_k(konst(5)) == 5);
    assert!(at_k(ident(5)) == 6);
    assert!(at_s(konst("s")) == "s");
    assert!(at_s(ident("s")) == "arg");
    assert!(at_f(konst(2.5)) == 2.5);
    assert!(at_f(ident(2.5)) == 0.5);
    assert!(at_b(konst(true)));
    assert!(!at_b(ident(true)));
    assert!(at_c(konst(new Cell(7))) == 7);
    assert!(at_c(ident(new Cell(7))) == 0);
    assert!(at_o(maker(Some("o"))) == "o");
    assert!(at_o(maker(None)) == "none");
    assert!(at_m(maker(41)) == 41);
    assert!(at_sc(scaled("s", 21)) == 42);
}

test("a shared body's closure through a let") {
    fill();
    let k = konst(5);
    assert!(k(6) == 5);
    let i = ident(5);
    assert!(i(6) == 6);
    let s = ident("s");
    assert!(s("t") == "t");
    let fl = konst(1.5);
    assert!(fl(0.25) == 1.5);
    let o = maker(Some("o"));
    assert!(match o() {
        Some(v) => v == "o",
        None => false,
    });
    let m = scaled(new Cell(0), 4);
    assert!(m(new Cell(1)) == 8);
    assert!(format("%i", k(0) + i(1)) == "6");
}

test("a shared body's closure called in place") {
    fill();
    assert!(konst(5)(6) == 5);
    assert!(ident(5)(6) == 6);
    assert!(ident("a")("b") == "b");
    assert!(maker(2.5)() == 2.5);
    assert!(scaled(true, 3)(false) == 6);
}

test("a shared body's closure using a bound, from another shared body") {
    fill();
    let s = shown(5);
    assert!(s(6) == "5/6");
    let t = shown("x");
    assert!(t("y") == "x/y");
    assert!(relay(5) == 5);
    assert!(relay("r") == "r");
    assert!(relay(new Cell(3)).v == 3);
    assert!(ground_in(1.5) == 3);
    assert!(ground_in("z") == 3);
}

test("a shared body's closure stored, nested, over an enum or a range") {
    fill();
    let fs = [konst(1), ident(2), konst(3)];
    assert!(fs[0](9) + fs[1](9) + fs[2](9) == 13);
    let mm = maker(maker(8));
    let m = mm();
    assert!(m() == 8);
    let io = ident(Some("a"));
    assert!(match io(None) {
        Some(v) => false,
        None => true,
    });
    assert!(match io(Some("b")) {
        Some(v) => v == "b",
        None => false,
    });
    let oi = ident(Some(4));
    assert!(match oi(Some(5)) {
        Some(v) => v == 5,
        None => false,
    });
    let r = maker(1..4);
    let n = 0;
    for i in r() {
        n = n + i;
    }
    assert!(n == 6);
}
