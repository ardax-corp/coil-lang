// Helper module for tests/modules/readonly_pair_class_import.hy (#1562).

readonly class Pt {
    pub x: float,
    pub y: float,
}

fn mid(Pt a, Pt b) -> Pt {
    return new Pt((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
}

fn far(int n) -> Pt {
    if n == 0 {
        return new Pt(0.0, 0.0);
    }
    let p = far(n - 1);
    return new Pt(p.x + 1.0, p.y);
}
