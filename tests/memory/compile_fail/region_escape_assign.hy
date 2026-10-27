// @expected E0140 — a region's object assigned to a local declared outside it.
use string::format;

fn main() {
    let last = "";
    region {
        let s = format("a%i", 1);
        last = s;
    }
}
