use gc::{collect, get, root, unroot, upgrade, weak};
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};

fn ephemeral_weak() {
    let r = root([1, 2, 3]);
    let inner = match get(r) {
        Option::Some(v) => v,
        Option::None => [1, 2, 3],
    };
    let w = weak(inner);
    unroot(r);
    return w;
}

fn main() {
    let w = ephemeral_weak();
    collect();
    let label = match upgrade(w) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    write_all(stdout(), to_bytes(label));
}
