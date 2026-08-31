// COI-108: nested method `?` with a different Ok payload must keep the
// inner ReturnPair (not JumpIfMatch a prematurely boxed heap enum).

class Enc {}

impl Enc {
    pub fn encode(int n) -> Result<Vec<byte>, string> {
        let out: Vec<byte> = Vec::new();
        out.push(n as byte);
        let m = n + 1;
        out.push(m as byte);
        return out;
    }

    pub fn encode_fail(int _n) -> Result<Vec<byte>, string> {
        raise "boom";
    }

    pub fn encode_into(int n) -> Result<int, string> {
        let bytes = self.encode(n)?;
        return len(bytes);
    }

    pub fn encode_first(int n) -> Result<byte, string> {
        let bytes = self.encode(n)?;
        return bytes[0];
    }

    pub fn encode_into_fail(int n) -> Result<int, string> {
        let bytes = self.encode_fail(n)?;
        return len(bytes);
    }
}

fn free_encode(int n) -> Result<Vec<byte>, string> {
    let out: Vec<byte> = Vec::new();
    out.push(n as byte);
    return out;
}

fn free_encode_into(int n) -> Result<int, string> {
    let bytes = free_encode(n)?;
    return len(bytes);
}

test("nested method try preserves Ok payload length") {
    let e = new Enc();
    let n = e.encode_into(10)?;
    assert(n == 2)?;
}

test("nested method try preserves Ok byte payload") {
    let e = new Enc();
    let b = e.encode_first(10)?;
    assert(b == (10 as byte))?;
}

test("nested method try propagates Err") {
    let e = new Enc();
    let r = e.encode_into_fail(1);
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(m) => m,
    };
    assert(msg == "boom")?;
}

test("nested free-fn try mismatched Result payload") {
    let n = free_encode_into(7)?;
    assert(n == 1)?;
}

class Client {}

impl Client {
    pub fn get() -> Result<int, string> {
        return self.send()?;
    }

    pub fn send() -> Result<int, string> {
        return self.request_send()?;
    }

    pub fn request_send() -> Result<int, string> {
        return 42;
    }
}

test("nested same-Result methods declared later") {
    let c = new Client();
    let n = c.get()?;
    assert(n == 42)?;
}

class ClientFail {}

impl ClientFail {
    pub fn get() -> Result<int, string> {
        return self.send()?;
    }

    pub fn send() -> Result<int, string> {
        return self.boom()?;
    }

    pub fn boom() -> Result<int, string> {
        raise "nope";
    }
}

test("forward same-Result methods propagate Err") {
    let c = new ClientFail();
    let r = c.get();
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(m) => m,
    };
    assert(msg == "nope")?;
}

class Counter {}

impl Counter {
    pub fn early() -> int {
        return self.late();
    }

    pub fn late() -> int {
        return 7;
    }
}

test("forward non-Result instance method call") {
    let c = new Counter();
    assert(c.early() == 7)?;
}

class EncFwd {}

impl EncFwd {
    pub fn encode_into(int n) -> Result<int, string> {
        let bytes = self.encode(n)?;
        return len(bytes);
    }

    pub fn encode(int n) -> Result<Vec<byte>, string> {
        let out: Vec<byte> = Vec::new();
        out.push(n as byte);
        return out;
    }
}

test("forward mismatched-Result method try") {
    let e = new EncFwd();
    let n = e.encode_into(9)?;
    assert(n == 1)?;
}

class Factory {}

impl Factory {
    pub fn make() -> int {
        return Factory::value();
    }

    pub static fn value() -> int {
        return 9;
    }
}

test("forward static method call from instance") {
    let f = new Factory();
    assert(f.make() == 9)?;
}
