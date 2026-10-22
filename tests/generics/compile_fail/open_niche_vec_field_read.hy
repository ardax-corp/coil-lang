// @expected E0801 — a generic class's `Vec<V>` field holds a niche `Option`
// boxed by the shared body; read at its ground type from ground code it
// would print an empty payload (#1896). Refused until open `V` holds the
// ground word (niche-vs-boxed step 3 turns this into an `@output` test).
use io::sync::{println};

class Slot<V> {
    pub v: V,
    pub vs: Vec<V>,
}

impl Slot<V> {
    pub fn put(V x) {
        self.v = x;
        self.vs.push(x);
    }

    pub fn at(int i) -> V {
        return self.vs[i];
    }
}

fn say(Option<string> o) -> string {
    return match o {
        Some(s) => "some " + s,
        None => "none",
    };
}

fn main() {
    let e: Vec<Option<string>> = Vec::new();
    let c = new Slot(None, e);
    let v: Option<string> = Some("one");
    c.put(v);
    println(say(c.at(0)))?;
    println(say(c.vs[0]))?;
}
