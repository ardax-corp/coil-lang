// Expected: E0413 — #886: a "w" mode writes, so `uses {read}` is not enough.
use io::open;

fn make(string path) -> bool uses {read} {
    return match open(path, "w") {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

fn main() {
    make("/tmp/x");
}
