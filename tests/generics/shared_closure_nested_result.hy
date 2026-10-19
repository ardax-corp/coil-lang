// Past the clone cap (E0804) a shared generic body returns a closure
// whose result holds its type parameter's value boxed inside an `Option`,
// a `Result` or a tuple: the closure's adapter rebuilds that result in its
// ground layout, as a direct call's is (#1593).
use tests::assert;

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

fn opt<T>(T x) -> unit -> Option<T> {
    return fn () use (x) => Some(x);
}

fn pair<T>(T x) -> int -> (T, int) {
    return fn (int n) use (x) => (x, n + 1);
}

fn res<T>(T x) -> bool -> Result<T, string> {
    return fn (bool ok) use (x) {
        if ok {
            return Ok(x);
        }
        return Err("no");
    };
}

fn flip<A, B>(A a) -> B -> (B, A) {
    return fn (B b) use (a) => (b, a);
}

fn at_o(unit -> Option<string> f) -> string {
    return match f() {
        Some(v) => v,
        None => "none",
    };
}

fn fill() {
    let opt_a = opt(new A(1));
    let opt_b = opt(new B(1));
    let opt_c = opt(new C(1));
    let opt_d = opt(new D(1));
    let opt_e = opt(new E(1));
    let opt_f = opt(new F(1));
    let opt_g = opt(new G(1));
    let opt_h = opt(new H(1));
    let pair_a = pair(new A(1));
    let pair_b = pair(new B(1));
    let pair_c = pair(new C(1));
    let pair_d = pair(new D(1));
    let pair_e = pair(new E(1));
    let pair_f = pair(new F(1));
    let pair_g = pair(new G(1));
    let pair_h = pair(new H(1));
    let res_a = res(new A(1));
    let res_b = res(new B(1));
    let res_c = res(new C(1));
    let res_d = res(new D(1));
    let res_e = res(new E(1));
    let res_f = res(new F(1));
    let res_g = res(new G(1));
    let res_h = res(new H(1));
    let flip_a = flip(new A(1))(1);
    let flip_b = flip(new B(1))(1);
    let flip_c = flip(new C(1))(1);
    let flip_d = flip(new D(1))(1);
    let flip_e = flip(new E(1))(1);
    let flip_f = flip(new F(1))(1);
    let flip_g = flip(new G(1))(1);
    let flip_h = flip(new H(1))(1);
}

fn options() {
    assert!(at_o(opt("s")) == "s");
    let oi = opt(5);
    assert!(match oi() {
        Some(v) => v + 1,
        None => 0,
    } == 6);
    let of = opt(2.5);
    assert!(match of() {
        Some(v) => v,
        None => 0.0,
    } == 2.5);
    let ob = opt(true);
    assert!(match ob() {
        Some(v) => v,
        None => false,
    });
    let oc = opt(new Cell(7));
    assert!(match oc() {
        Some(c) => c.v,
        None => 0,
    } == 7);
}

test("a shared body's closure returning an Option of its type parameter") {
    fill();
    options();
}

fn tuples() {
    let pi = pair(9);
    let (i, n) = pi(1);
    assert!(i == 9 && n == 2);
    let ps = pair("ps");
    let (s, m) = ps(2);
    assert!(s == "ps" && m == 3);
    let pc = pair(new Cell(4));
    let (c, k) = pc(3);
    assert!(c.v == 4 && k == 4);
    let fi = flip(1);
    let (b, a) = fi("x");
    assert!(b == "x" && a == 1);
    let fs = flip("y");
    let (q, r) = fs(2.5);
    assert!(q == 2.5 && r == "y");
}

test("a shared body's closure returning a tuple of its type parameter") {
    fill();
    tuples();
}

fn results() {
    let ri = res(6);
    assert!(match ri(true) {
        Ok(v) => v,
        Err(e) => -1,
    } == 6);
    assert!(match ri(false) {
        Ok(v) => "ok",
        Err(e) => e,
    } == "no");
    let rs = res("o");
    assert!(match rs(true) {
        Ok(v) => v,
        Err(e) => e,
    } == "o");
    assert!(match rs(false) {
        Ok(v) => v,
        Err(e) => e,
    } == "no");
}

test("a shared body's closure returning a Result of its type parameter") {
    fill();
    results();
}
