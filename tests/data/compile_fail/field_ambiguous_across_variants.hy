// Expected: E0119 — `x` is a field of two variants of `Two`; narrow with `match`
// before reading it.
enum Two {
    A { x: int, y: int },
    B { x: string, z: int },
}

fn get_x(Two p) -> int {
    return p.x;
}

fn main() {}
