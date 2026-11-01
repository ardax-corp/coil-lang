// `?` applied twice in one expression to a nested `Result` (#1166).
use tests::assert;

fn nested(Result<Result<int, string>, string> r) -> Result<int, string> {
    return (r?)? + 1;
}

fn nested_let(Result<Result<int, string>, string> r) -> Result<int, string> {
    let v = (r?)? * 2;
    return Result::Ok(v);
}

test("`(r?)?` on a nested Result") {
    assert!(nested(Result::Ok(Result::Ok(4))) == Result::Ok(5));
    assert!(nested(Result::Ok(Result::Err("in"))) == Result::Err("in"));
    assert!(nested(Result::Err("out")) == Result::Err("out"));
}

test("`(r?)?` as a let initializer") {
    assert!(nested_let(Result::Ok(Result::Ok(4))) == Result::Ok(8));
    assert!(nested_let(Result::Ok(Result::Err("in"))) == Result::Err("in"));
    assert!(nested_let(Result::Err("out")) == Result::Err("out"));
}
