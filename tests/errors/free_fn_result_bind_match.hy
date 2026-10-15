// A `Result` from a free function, bound to a local before the `match`,
// keeps its heap enum payload.
use tests::assert;

enum Node {
    Obj { v: int },
}

fn make_ok() -> Result<Node, string> {
    return Node::Obj { v: 42 };
}

test("bind a free fn result then match its payload") {
    let r = make_ok();
    let v = match r {
        Result::Ok(n) => match n {
            Node::Obj { v } => v,
        },
        Result::Err(_) => -1,
    };
    assert!(v == 42);
}
