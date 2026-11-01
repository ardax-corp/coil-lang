// GC roots and weak references: `root`, `get`, `weak`, `upgrade`, `unroot`.
//
// @output pinned\npinned

use gc::{get, root, unroot, upgrade, weak};
use io::sync::print;
use string::format;

fn main() {
    let r = root("pinned");
    let inner = match get(r) {
        Option::Some(s) => s,
        Option::None => "gone",
    };
    let w = weak(inner);
    let label = match upgrade(w) {
        Option::Some(s) => s,
        Option::None => "gone",
    };
    let _ = print(label);
    let taken = match unroot(r) {
        Option::Some(s) => s,
        Option::None => "gone",
    };
    let _ = print(format("\n%s", taken));
}
