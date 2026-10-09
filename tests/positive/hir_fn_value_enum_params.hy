// A function value whose parameters are enums: each enum argument goes
// through `CallIndirect` as its one word (a niche enum as its niche word,
// any other boxed), as the AST's escaped-function ABI.
use string::format;

enum Shape {
    Circle(int),
    Rect(int, int),
    Dot,
}

fn f(Option<int> o) -> int {
    return match o {
        Option::Some(x) => x,
        Option::None => 7,
    };
}

fn g(Option<string> o) -> string {
    return match o {
        Option::Some(x) => x,
        Option::None => "none",
    };
}

fn h(Result<int, string> r) -> int {
    return match r {
        Result::Ok(x) => x,
        Result::Err(e) => e.len(),
    };
}

fn area(Shape s) -> int {
    return match s {
        Shape::Circle(r) => 3 * r * r,
        Shape::Rect(w, l) => w * l,
        Shape::Dot => 0,
    };
}

fn twice(int x, Option<int> o) -> Option<int> {
    return match o {
        Option::Some(y) => Option::Some(x * y),
        Option::None => Option::None,
    };
}

fn apply(Option<string> -> string f, Option<string> o) -> string {
    return f(o);
}

fn ok(Result<string, string> r) -> Option<string> {
    return match r {
        Result::Ok(x) => Option::Some(x),
        Result::Err(_) => Option::None,
    };
}

test("named functions as values") {
    let a = f;
    let b = g;
    let c = h;
    let d = area;
    let e = twice;
    let t = e(3, Option::Some(5));
    let tv = match t {
        Option::Some(v) => v,
        Option::None => -1,
    };
    let o = Option::Some(11);
    let r: Result<int, string> = Result::Err("abcd");
    let s = format(
        "%i,%i,%s,%s,%i,%i,%i,%i,%i,%i,%i",
        a(Option::Some(3)),
        a(Option::None),
        b(Option::Some("x")),
        b(Option::None),
        c(Result::Ok(5)),
        c(r),
        d(Shape::Circle(2)),
        d(Shape::Rect(2, 3)),
        d(Shape::Dot),
        tv,
        a(o),
    );
    assert(s == "3,7,x,none,5,4,12,6,0,15,11", s)?;
}

test("lambdas and higher-order calls") {
    let k = fn (Option<string> o) => match o {
        Option::Some(s) => s + "!",
        Option::None => "-",
    };
    let m = ok;
    let n = match m(Result::Ok("y")) {
        Option::Some(s) => s,
        Option::None => "nn",
    };
    let p = match m(Result::Err("e")) {
        Option::Some(s) => s,
        Option::None => "nn",
    };
    let s = format(
        "%s,%s,%s,%s,%s",
        k(Option::Some("a")),
        k(Option::None),
        apply(k, Option::Some("b")),
        n,
        p,
    );
    assert(s == "a!,-,b!,y,nn", s)?;
}
