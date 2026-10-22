// @expected E0126 — a readonly class has no `drop`: its instances are values with no identity (#1558).
readonly class Handle {
    pub fd: int,
}

impl Handle {
    fn drop() {}
}

fn main() {
    let h = new Handle(3);
}
