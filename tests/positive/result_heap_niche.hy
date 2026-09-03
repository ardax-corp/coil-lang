use gc::{collect};

class Node {
    pub v: int,
}

fn take(Result<Node, Node> r) -> int {
    return match r {
        Result::Ok(n) => n.v,
        Result::Err(e) => -e.v,
    };
}

fn through(Result<Node, Node> r) -> Result<Node, Node> {
    let n = r?;
    return Result::Ok(n);
}

test("result of two classes niches Ok and Err") {
    let a = new Node(7);
    let b = new Node(4);
    assert(take(Result::Ok(a)) == 7)?;
    assert(take(Result::Err(b)) == -4)?;
}

test("result heap try and coalesce") {
    let ok = through(Result::Ok(new Node(3)));
    let err = through(Result::Err(new Node(9)));
    assert((ok ?? new Node(0)).v == 3)?;
    assert((err ?? new Node(11)).v == 11)?;
}

test("ok is not err even with the same object") {
    let n = new Node(1);
    let ok: Result<Node, Node> = Result::Ok(n);
    let err: Result<Node, Node> = Result::Err(n);
    assert(ok == ok)?;
    assert(err == err)?;
    assert(!(ok == err))?;
    assert(Result::Ok(n) == Result::Ok(n))?;
    assert(Result::Err(n) == Result::Err(n))?;
    assert(!(Result::Ok(n) == Result::Err(n)))?;
}

test("err payload survives gc") {
    let r = Result::Err(new Node(9));
    collect();
    assert(take(r) == -9)?;
}

fn take_int_ok(Result<int, Node> r) -> int {
    return match r {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

fn take_int_err(Result<Node, int> r) -> int {
    return match r {
        Result::Ok(n) => n.v,
        Result::Err(e) => e,
    };
}

test("mixed immediate Result stays boxed") {
    assert(take_int_ok(Result::Ok(41)) == 41)?;
    assert(take_int_err(Result::Err(8)) == 8)?;
}
