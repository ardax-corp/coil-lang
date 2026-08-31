use gc::{get, root, unroot, upgrade, weak};
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

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
    write_all(stdout(), to_bytes(label));
    let taken = match unroot(r) {
        Option::Some(s) => s,
        Option::None => "gone",
    };
    write_all(stdout(), to_bytes(format("\n%s", taken)));
}
