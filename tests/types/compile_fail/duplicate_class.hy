// @expected E0119 — a second `class A` must not silently replace the first (#1169).
class A {
    pub x: int,
}

class A {
    pub y: string,
}

fn main() {
    let a = new A(1);
}
