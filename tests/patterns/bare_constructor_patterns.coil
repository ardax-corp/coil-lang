// Bare constructor names as patterns (#1352): `Some(v)`, `None`, `Ok(x)`,
// `Err(e)` and a user enum's cases resolve like bare constructors in
// expressions. A lowercase name, or one that names no case, stays a binding.
use tests::assert;

enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

enum Light {
    Red,
    Amber,
    Green,
}

fn opt_or(Option<int> o, int fallback) -> int {
    return match o {
        Some(v) => v,
        None => fallback,
    };
}

fn res_code(Result<int, string> r) -> int {
    return match r {
        Ok(n) => n,
        Err(_) => -1,
    };
}

fn area(Shape s) -> int {
    return match s {
        Dot => 0,
        Circle(r) => 3 * r * r,
        Rect { w, h } => w * h,
    };
}

fn wide(Shape s) -> bool {
    return match s {
        Rect { w: 10, h } => h > 0,
        default => false,
    };
}

fn next(Light l) -> Light {
    return match l {
        Red => Green,
        Green => Amber,
        Amber => Red,
    };
}

fn light_id(Light l) -> int {
    return match l {
        Red => 0,
        Amber => 1,
        Green => 2,
    };
}

fn nested(Result<Option<int>, string> r) -> int {
    return match r {
        Ok(Some(v)) => v,
        Ok(None) => 0,
        Err(_) => -1,
    };
}

fn mixed(Option<Shape> o) -> int {
    return match o {
        Some(Circle(r)) => r,
        Some(Shape::Rect { w, h: _ }) => w,
        Some(Dot) => 1,
        None => -1,
    };
}

fn if_let_some(Option<int> o) -> int {
    if let Some(v) = o {
        return v;
    }
    return -1;
}

fn if_let_unit(Option<int> o) -> bool {
    if let None = o {
        return true;
    }
    return false;
}

fn if_let_record(Shape s) -> int {
    if let Rect { w, h } = s {
        return w + h;
    } else if let Circle(r) = s {
        return r;
    }
    return 0;
}

fn drain(Vec<int> v) -> int {
    let total = 0;
    while let Some(x) = v.pop() {
        total = total * 10 + x;
    }
    return total;
}

fn binding_stays(Option<int> o) -> int {
    return match o {
        Some(v) => v,
        other => 7,
    };
}

fn upper_binding(int n) -> int {
    return match n {
        0 => 0,
        Other => Other + 1,
    };
}

test("bare Option patterns") {
    assert!(opt_or(Option::Some(4), 0) == 4);
    assert!(opt_or(Option::None, 9) == 9);
}

test("bare Result patterns") {
    assert!(res_code(Result::Ok(3)) == 3);
    assert!(res_code(Result::Err("no")) == -1);
}

test("bare user enum tuple, record and unit cases") {
    assert!(area(Shape::Dot) == 0);
    assert!(area(Shape::Circle(2)) == 12);
    assert!(area(Shape::Rect { w: 3, h: 4 }) == 12);
    assert!(wide(Shape::Rect { w: 10, h: 1 }));
    assert!(!wide(Shape::Rect { w: 9, h: 1 }));
}

test("bare unit cases in patterns and bodies") {
    assert!(light_id(next(Light::Red)) == 2);
    assert!(light_id(next(Light::Green)) == 1);
    assert!(light_id(next(Light::Amber)) == 0);
}

test("nested bare patterns") {
    assert!(nested(Result::Ok(Option::Some(5))) == 5);
    assert!(nested(Result::Ok(Option::None)) == 0);
    assert!(nested(Result::Err("x")) == -1);
}

test("bare and qualified patterns mix") {
    assert!(mixed(Option::Some(Shape::Circle(6))) == 6);
    assert!(mixed(Option::Some(Shape::Rect { w: 2, h: 3 })) == 2);
    assert!(mixed(Option::Some(Shape::Dot)) == 1);
    assert!(mixed(Option::None) == -1);
}

test("if let and while let take bare patterns") {
    assert!(if_let_some(Option::Some(8)) == 8);
    assert!(if_let_some(Option::None) == -1);
    assert!(if_let_unit(Option::None));
    assert!(!if_let_unit(Option::Some(1)));
    assert!(if_let_record(Shape::Rect { w: 1, h: 2 }) == 3);
    assert!(if_let_record(Shape::Circle(5)) == 5);
    assert!(if_let_record(Shape::Dot) == 0);
    let v: Vec<int> = [1, 2, 3];
    assert!(drain(v) == 321);
}

test("non-case names stay bindings") {
    assert!(binding_stays(Option::None) == 7);
    assert!(upper_binding(4) == 5);
}
