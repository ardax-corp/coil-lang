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

class Pair<T> {
    pub a: T,
    pub b: T,
}

enum Gv {
    P(Pair<int>),
    E,
}

fn make_gv(int x) -> Gv {
    return Gv::P(new Pair(x, x + 1));
}

fn pair_sum(Pair<int> p) -> int {
    return p.a + p.b;
}

fn gv_value(Gv g) -> int {
    return match g {
        Gv::P(p) => pair_sum(p) + p.a,
        Gv::E => 0,
    };
}
