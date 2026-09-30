// examples/collections_map.hy — HashMap insert / get_or / update
//
// Output: A,2

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};
use collections::map::HashMap;

fn main() {
    let m = HashMap::new();
    m.insert(1, "a");
    m.insert(2, "b");
    m.insert(1, "A");
    write_all(stdout(), to_bytes(format("%s,%i", m.get_or(1, "?"), m.size())));
}
