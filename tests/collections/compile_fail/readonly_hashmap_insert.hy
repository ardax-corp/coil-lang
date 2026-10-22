// @expected E0107 — `HashMap::insert` mutates `self`, so a `readonly` map cannot call it; `get` can (#1558).
use collections::map::HashMap;

fn main() {
    let m: HashMap<string, int> = HashMap::new();
    m.insert("a", 1);
    let r = readonly m;
    let x = r.get("a", 0);
    r.insert("b", 2);
}
