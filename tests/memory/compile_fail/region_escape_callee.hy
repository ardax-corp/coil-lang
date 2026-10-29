// @expected E0140 — a region's object passed to a function that keeps it.
use string::format;

static let kept: Vec<string> = Vec::new();

fn keep(string s) {
    kept.push(s);
}

fn look(string s) -> int {
    return s.len();
}

fn main() {
    region {
        let n = look(format("f%i", 6));
        keep(format("e%i", n));
    }
}
