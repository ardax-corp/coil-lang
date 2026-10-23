// examples/collections_map.hy — HashMap insert, overwrite and lookup
//
// @output A,2

use io::sync::print;
use string::format;
use collections::map::HashMap;

fn main() {
    let m = HashMap::new();
    m.insert(1, "a");
    m.insert(2, "b");
    m.insert(1, "A");
    let _ = print(format("%s,%i", m.get_or(1, "?"), m.size()));
}
