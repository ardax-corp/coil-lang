// A trait imported from another module can be implemented for a local
// class and called as a method, function-style, and via its default (#522).

use trait_import_codec::{Node, DecodeError, FromNode};

class Config {
    pub port: int,
    pub name: string,
}

impl FromNode for Config {
    pub fn from_node(Config proto, Node n) -> Result<Config, DecodeError> {
        if n.i < 0 {
            return Result::Err(DecodeError::Missing { key: "port" });
        }
        return Result::Ok(new Config(n.i, n.s));
    }
}

fn port_of(Result<Config, DecodeError> r) -> int {
    let p = match r {
        Result::Ok(c) => c.port,
        Result::Err(_) => -1,
    };
    return p;
}

test("method call on an imported trait") {
    assert(port_of(new Config(0, "").from_node(new Node(3, "abc"))) == 3)?;
    assert(port_of(new Config(0, "").from_node(new Node(-1, "abc"))) == -1)?;
}

test("function-style call on an imported trait") {
    assert(port_of(from_node(new Config(0, ""), new Node(4, "abc"))) == 4)?;
}

test("default method from an imported trait") {
    assert(new Config(0, "").port_or(new Node(-1, "x"), 42) == 42)?;
}
