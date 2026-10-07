class Req {
    pub path: string,
    pub weight: int,
}

class Resp {
    pub code: int,
}

trait Handler<H> {
    fn handle(H self, Req req, int extra) -> Resp {}
}

class Hello {
    pub base: int,
}

impl Handler for Hello {
    fn handle(Hello self, Req req, int extra) -> Resp {
        return new Resp(self.base + len(req.path) * req.weight + extra);
    }
}

fn serve(Handler handler, int extra) -> int {
    let r = new Req("/abc", 2);
    let resp = handle(handler, r, extra);
    return resp.code;
}

fn make(int base) -> Handler {
    return new Hello(base);
}

test("a trait object method takes further arguments") {
    assert(serve(new Hello(200), 1) == 209)?;
}

test("a trait object from a call stages through a temp") {
    // A `new` argument here trips the AST (coil-lang#749).
    let q = new Req("/x", 3);
    let resp = handle(make(100), q, 0);
    assert(resp.code == 106)?;
}

test("a nested trait object call keeps the operands below it") {
    let q = new Req("ab", 1);
    let total = 1 + handle(make(10), q, 5).code;
    assert(total == 18)?;
}
