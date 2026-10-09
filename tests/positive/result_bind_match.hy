// COI-106: method Option/Result returns must box before local bind + match.

class Svc {}

enum Node {
    Obj { v: int },
}

impl Svc {
    pub fn decode() -> Result<Node, string> {
        return Node::Obj { v: 42 };
    }

    pub fn fail() -> Result<Node, string> {
        raise "boom";
    }

    pub fn maybe(int flag) -> Option<int> {
        if flag == 0 {
            return Option::None;
        }
        return Option::Some(flag);
    }
}

test("method result bind then match") {
    let s = new Svc();
    let r = s.decode();
    let ok = match r {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
    assert(ok)?;
}

test("method result bind preserves nested Ok payload") {
    let s = new Svc();
    let r = s.decode();
    let v = match r {
        Result::Ok(n) => match n {
            Node::Obj { v } => v,
        },
        Result::Err(_) => -1,
    };
    assert(v == 42)?;
}

test("method result Err bind then match") {
    let s = new Svc();
    let r = s.fail();
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    assert(msg == "boom")?;
}

test("method option Some bind then match") {
    let s = new Svc();
    let o = s.maybe(7);
    let v = match o {
        Option::Some(v) => v,
        Option::None => -1,
    };
    assert(v == 7)?;
}

test("method option None bind then match") {
    let s = new Svc();
    let o = s.maybe(0);
    let is_none = match o {
        Option::Some(_) => false,
        Option::None => true,
    };
    assert(is_none)?;
}
