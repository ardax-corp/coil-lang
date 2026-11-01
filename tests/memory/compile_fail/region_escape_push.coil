// @expected E0140 — a region's object pushed onto a vector made before it.
use string::format;

fn main() {
    let all: Vec<string> = Vec::new();
    region {
        all.push(format("d%i", 4));
    }
}
