// `Option` / `Result` values whose type mentions a type parameter cross
// generic boundaries in the boxed layout: generic code cannot know `T`, while
// concrete code may use a pointer niche (COI-92) for the same type. Free
// generic fns, calls through a trait bound (mono and dictionary), and
// default trait bodies convert at the boundary. Non-`T` args to a shared
// generic body pass unboxed.

class Node {
    pub i: int,
}

class Config {
    pub port: int,
}

trait FromNode<T> {
    fn from_node(T proto, Node n) -> Result<T, string> {}
    fn pick(T proto, Option<T> o) -> T {}
    fn maybe(T proto, int k) -> Option<T> {}

    // Default bodies are generic: they reach siblings through the dictionary.
    fn port_or(T proto, Node n, int d) -> int {
        return match proto.from_node(n) {
            Result::Ok(_) => n.i,
            Result::Err(_) => d,
        };
    }

    fn pick_or_self(T proto, int k) -> T {
        return proto.pick(proto.maybe(k));
    }
}

impl FromNode for Config {
    pub fn from_node(Config proto, Node n) -> Result<Config, string> {
        if n.i < 0 {
            return Result::Err("neg");
        }
        return Result::Ok(new Config(n.i));
    }

    pub fn pick(Config proto, Option<Config> o) -> Config {
        return match o {
            Option::Some(c) => c,
            Option::None => proto,
        };
    }

    pub fn maybe(Config proto, int k) -> Option<Config> {
        if k > 0 {
            return Option::Some(new Config(k));
        }
        return Option::None;
    }
}

fn ok_of<T: FromNode>(T proto, Node n) -> int {
    let r = match proto.from_node(n) {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    return r;
}

fn via_two_generics<T: FromNode>(T proto, Node n) -> int {
    return ok_of(proto, n);
}

fn pick_through<T: FromNode>(T proto, Option<T> o) -> T {
    return proto.pick(o);
}

fn maybe_through<T: FromNode>(T proto, int k) -> int {
    let r = match proto.maybe(k) {
        Option::Some(_) => 1,
        Option::None => 0,
    };
    return r;
}

test("Result<T, E> return through a bound") {
    assert(ok_of(new Config(0), new Node(5)) == 1)?;
    assert(ok_of(new Config(0), new Node(-5)) == 0)?;
}

test("through two generic layers") {
    assert(via_two_generics(new Config(0), new Node(5)) == 1)?;
    assert(via_two_generics(new Config(0), new Node(-5)) == 0)?;
}

test("default method calls a sibling through the dictionary") {
    assert(new Config(0).port_or(new Node(7), 42) == 7)?;
    assert(new Config(0).port_or(new Node(-1), 42) == 42)?;
}

test("Option<T> param through a bound") {
    assert(pick_through(new Config(1), Option::Some(new Config(9))).port == 9)?;
    assert(pick_through(new Config(1), Option::None).port == 1)?;
}

test("Option<T> return through a bound") {
    assert(maybe_through(new Config(0), 3) == 1)?;
    assert(maybe_through(new Config(0), -3) == 0)?;
}

test("default method passes Option<T> between siblings") {
    assert(new Config(1).pick_or_self(4).port == 4)?;
    assert(new Config(1).pick_or_self(-4).port == 1)?;
}

test("ground calls stay concrete") {
    let p = match new Config(0).from_node(new Node(8)) {
        Result::Ok(c) => c.port,
        Result::Err(_) => -1,
    };
    assert(p == 8)?;
    assert(new Config(1).pick(Option::Some(new Config(6))).port == 6)?;
}

fn wrap_res<T>(T x, bool bad) -> Result<T, string> {
    if bad {
        return Result::Err("bad");
    }
    return Result::Ok(x);
}

fn res_port(bool bad) -> int {
    let r = match wrap_res(new Config(8), bad) {
        Result::Ok(c) => c.port,
        Result::Err(_) => -1,
    };
    return r;
}

test("generic fn returning Result<T, E> at a heap T") {
    assert(res_port(false) == 8)?;
    assert(res_port(true) == -1)?;
}

fn port_or<T>(Result<T, string> r, T d) -> T {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => d,
    };
}

fn ok_cfg(int p) -> Result<Config, string> {
    if p < 0 {
        return Result::Err("neg");
    }
    return Result::Ok(new Config(p));
}

test("generic param Result<T, E> from a niche caller") {
    assert(port_or(ok_cfg(5), new Config(9)).port == 5)?;
    assert(port_or(ok_cfg(-1), new Config(9)).port == 9)?;
}
