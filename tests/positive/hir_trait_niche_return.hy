// A ground trait call whose result is a niche `Result` / `Option` lowers
// from HIR: the instance entry returns that word as the call site reads it.
class Cfg {
    pub port: int,
}

enum ParseError {
    Empty,
}

trait Parse<T> {
    static fn parse(int raw) -> Result<T, ParseError> {}
}

impl Parse for Cfg {
    pub static fn parse(int raw) -> Result<Cfg, ParseError> {
        if raw <= 0 {
            return Result::Err(ParseError::Empty);
        }
        return Result::Ok(new Cfg(raw));
    }
}

trait Lookup<T> {
    static fn find(int key) -> Option<T> {}
}

impl Lookup for Cfg {
    pub static fn find(int key) -> Option<Cfg> {
        if key == 1 {
            return Option::Some(new Cfg(80));
        }
        return Option::None;
    }
}

fn port_or(int raw, int fallback) -> int {
    return match Cfg::parse(raw) {
        Result::Ok(c) => c.port,
        Result::Err(_) => fallback,
    };
}

fn twice(int raw) -> Result<int, ParseError> {
    let a: Cfg = Cfg::parse(raw)?;
    let b: Cfg = Cfg::parse(raw + 1)?;
    return Result::Ok(a.port + b.port);
}

test("niche Result from a static trait call") {
    assert(port_or(8080, 0) == 8080)?;
    assert(port_or(0, 7) == 7)?;
    let ok = match twice(10) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
    assert(ok == 21)?;
}

test("niche Option from a static trait call") {
    if let Option::Some(c) = Cfg::find(1) {
        assert(c.port == 80)?;
    } else {
        assert(false)?;
    }
    let missing = match Cfg::find(2) {
        Option::Some(_) => 1,
        Option::None => 0,
    };
    assert(missing == 0)?;
}
