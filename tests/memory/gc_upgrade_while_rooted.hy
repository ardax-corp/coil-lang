// Output: some
use gc::{get, root, weak, upgrade};
use io::{stdout, write};
use string::to_bytes;

fn main() {
    let r = root([7, 8]);
    let inner = match get(r) {
        Option::Some(v) => v,
        Option::None => {
            return;
        },
    };
    let w = weak(inner);
    let out = match upgrade(w) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    let _ = write(stdout(), to_bytes(out));
}
