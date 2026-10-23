// Provider for tests/modules/same_module_class_types.hy: an enum whose payload
// names a class declared in this module (#1983).

class Box {
    pub v: int,
}

enum Ev {
    B(Box),
    N,
}

fn make_ev(int x) -> Ev {
    return Ev::B(new Box(x));
}

fn unbox(Box b) -> int {
    return b.v;
}

fn ev_value(Ev e) -> int {
    return match e {
        Ev::B(b) => unbox(b) + b.v,
        Ev::N => 0,
    };
}
