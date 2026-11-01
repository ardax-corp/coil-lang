// `raise`, `?` through helpers, `Option` early return, `??` on `Option`, `?.` through an optional field and `unwrap_or` on a `Result`.
// @output 4 err:bad 0 7 none 3 42\n
use io::sync::println;
use string::format;

fn parse(string s) -> Result<int, string> {
    if s == "bad" {
        raise "bad";
    }
    return s.len();
}

fn twice(string s) -> Result<int, string> {
    let n = parse(s)?;
    return n * 2;
}

fn head(Vec<int> v) -> Option<int> {
    if v.len() == 0 {
        return Option::None;
    }
    return Option::Some(v[0]);
}

fn head_plus(Vec<int> v) -> Option<int> {
    let h = head(v)?;
    return Option::Some(h + 1);
}

class Cfg {
    pub port: Option<int>,
}

fn main() {
    let a = twice("ab").unwrap_or(-1);
    let e = match twice("bad") {
        Result::Ok(v) => format("%i", v),
        Result::Err(m) => "err:" + m,
    };
    let empty: Vec<int> = Vec::new();
    let z = head_plus(empty) ?? 0;
    let v: Vec<int> = Vec::new();
    v.push(6);
    let hp = head_plus(v) ?? 0;
    let cfg = new Cfg(Option::None);
    let cfg2 = new Cfg(Option::Some(42));
    let port = match cfg.port {
        Option::Some(p) => format("%i", p),
        Option::None => "none",
    };
    let port2 = cfg2.port ?? 0;
    let o: Option<Cfg> = Option::Some(cfg2);
    let np = o?.port;
    let n = match np {
        Option::Some(x) => match x {
            Option::Some(y) => y,
            Option::None => 0,
        },
        Option::None => 0,
    };
    let _ = println(format("%i %s %i %i %s %i %i", a, e, z, hp, port, port2 - 39, n));
}
