// Provider for tests/modules/same_module_class_types.hy: bodies that call a
// later-declared fn or a recursive method whose signature names a class from
// this module (#1980).

class Opts {
    pub x: bool,
}

class Guard {
    pub n: int,
}

fn make() -> bool {
    return make_with(new Opts(true));
}

fn make_guard() -> int {
    return match guard_with(new Opts(true)) {
        Ok(g) => g.n,
        Err(_) => 0,
    };
}

fn make_with(Opts o) -> bool {
    return o.x;
}

fn guard_with(Opts o) -> Result<Guard, string> {
    if o.x {
        return Ok(new Guard(7));
    }
    return Err("no");
}

class Node {
    pub kids: Vec<Node>,
}

class Spec {
    pub subs: Vec<Spec>,
}

impl Spec {
    pub fn build() -> Node {
        let n = new Node(Vec::new());
        for s in self.subs {
            let k = s.build();
            n.kids.push(k);
        }
        return n;
    }
}

fn spec_kids() -> int {
    let s = new Spec(Vec::new());
    s.subs.push(new Spec(Vec::new()));
    s.subs.push(new Spec(Vec::new()));
    return s.build().kids.len();
}

class Cell<T> {
    pub v: T,
}

fn cell_value() -> int {
    return take_cell(new Cell(5));
}

fn take_cell(Cell<int> c) -> int {
    return c.v;
}
