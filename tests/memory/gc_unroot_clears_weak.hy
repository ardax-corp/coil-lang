// Output: none
use gc::{collect, get, root, unroot, weak, upgrade};
use io::{stdout, write};
use string::to_bytes;

fn only_weak() {
    let r = root("temp");
    let v = match get(r) {
        Option::Some(x) => x,
        Option::None => {
            return weak("");
        },
    };
    let w = weak(v);
    let _ = unroot(r);
    return w;
}

fn main() {
    let w = only_weak();
    let _ = collect();
    let out = match upgrade(w) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    write(stdout(), to_bytes(out));
}
