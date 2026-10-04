// Nested payload patterns lower from HIR: the arms of each outer variant
// regroup into one arm over an inner `match` on the payload. A variant
// that is not exhaustive on its own (a later catch-all covers it) keeps
// the AST codegen. The language harness also runs under `--hir`, so each
// case pins both codegens.
enum Shape {
    Dot,
    Line(int),
    Pair(Option<int>),
}

fn lookup(int k) -> Result<Option<int>, string> {
    if k < 0 {
        return Result::Err("negative");
    }
    if k == 0 {
        return Result::Ok(Option::None);
    }
    return Result::Ok(Option::Some(k * 10));
}

fn describe(int k) -> int {
    return match lookup(k) {
        Result::Ok(Option::None) => 0,
        Result::Ok(Option::Some(n)) => n,
        Result::Err(_) => 0 - 1,
    };
}

fn depth(Option<Option<int>> o) -> int {
    return match o {
        Option::Some(Option::Some(x)) => x,
        Option::Some(Option::None) => 1,
        Option::None => 0,
    };
}

fn size(Shape s) -> int {
    return match s {
        Shape::Pair(Option::Some(n)) => n * 2,
        Shape::Line(n) => n,
        Shape::Pair(Option::None) => 0 - 2,
        Shape::Dot => 0,
    };
}

fn with_default(Option<Option<int>> o) -> int {
    return match o {
        Option::Some(Option::Some(x)) => x,
        default => 0 - 7,
    };
}

test("result of option") {
    assert(describe(0) == 0)?;
    assert(describe(4) == 40)?;
    assert(describe(0 - 3) == 0 - 1)?;
}

test("option of option") {
    assert(depth(Option::Some(Option::Some(9))) == 9)?;
    assert(depth(Option::Some(Option::None)) == 1)?;
    assert(depth(Option::None) == 0)?;
}

test("interleaved user enum arms") {
    assert(size(Shape::Pair(Option::Some(5))) == 10)?;
    assert(size(Shape::Pair(Option::None)) == 0 - 2)?;
    assert(size(Shape::Line(3)) == 3)?;
    assert(size(Shape::Dot) == 0)?;
}

test("catch-all after a nested arm") {
    assert(with_default(Option::Some(Option::Some(2))) == 2)?;
    assert(with_default(Option::Some(Option::None)) == 0 - 7)?;
    assert(with_default(Option::None) == 0 - 7)?;
}
