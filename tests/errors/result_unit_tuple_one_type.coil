// @output ok:ok:err:ok:boom\ntrue true false true\n() () ()\n2 1 2 2\nok ok err ok\n
// `()` and `unit` are one type: `Result<(), E>` and `Result<unit, E>` mix
// freely (passed, returned, `?`, `==`, matched, stored, generic), and `()`
// shows as `()` whichever spelling made it.
use io::{stdout, write};
use string::{format, to_bytes};

fn tup(bool fail) -> Result<(), string> {
    if fail {
        return Result::Err("boom");
    }
    return Result::Ok(());
}

fn uni(bool fail) -> Result<unit, string> {
    if fail {
        return Result::Err("boom");
    }
    return Result::Ok(());
}

fn name_u(Result<unit, string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(m) => m,
    };
}

fn name_t(Result<(), string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(_) => "err",
    };
}

fn tup_via_uni(bool fail) -> Result<(), string> {
    uni(fail)?;
    return Result::Ok(());
}

fn uni_via_tup(bool fail) -> Result<unit, string> {
    let r = tup(fail);
    r?;
    return r;
}

fn unit_fn() {}

fn ident<T>(T x) -> T {
    return x;
}

class Holder {
    pub a: Result<unit, string>,
    pub b: Result<(), string>,
}

fn passing() -> string {
    // One spelling passed and returned where the other is expected; `?`
    // across them.
    return format(
        "%s:%s:%s:%s:%s",
        name_u(tup(false)),
        name_t(uni(false)),
        name_t(uni(true)),
        name_u(tup_via_uni(false)),
        name_u(uni_via_tup(true)),
    );
}

fn equality() -> string {
    let x: Result<(), string> = Result::Ok(());
    let y: Result<unit, string> = Result::Ok(unit_fn());
    let e = tup(false) == uni(false);
    return format(
        "%v %v %v %v",
        x == y,
        e,
        tup(true) == uni(false),
        uni(true) == Result::Err("boom"),
    );
}

fn shows() -> string {
    let shown = match tup(false) {
        Result::Ok(u) => show(u),
        Result::Err(m) => m,
    };
    return format("%s %s %s", show(()), show(unit_fn()), shown);
}

fn stored() -> string {
    let x: Result<(), string> = Result::Ok(());
    let y: Result<unit, string> = Result::Ok(unit_fn());
    let vu: Vec<Result<unit, string>> = [];
    vu.push(tup(false));
    vu.push(Result::Ok(()));
    vu.push(tup(true));
    let vt: Vec<Result<(), string>> = [uni(false), uni(true)];
    vt[1] = x;
    let h = new Holder(tup(false), uni(true));
    let ok = 0;
    for r in vu {
        if r == Result::Ok(()) {
            ok = ok + 1;
        }
    }
    let okt = 0;
    for r in vt {
        if r == Result::Ok(()) {
            okt = okt + 1;
        }
    }
    let both = 0;
    if h.a == Result::Ok(()) {
        both = both + 1;
    }
    if h.b == Result::Ok(()) {
        both = both + 1;
    }
    h.b = y;
    if h.b == Result::Ok(()) {
        both = both + 1;
    }
    return format("%i %i %i %i", ok, vu.len() - ok, okt, both);
}

fn generic() -> string {
    // Generic instantiation at `()` and at `unit`.
    let g1: Result<unit, string> = ident(tup(false));
    let g2: Result<(), string> = ident(uni(false));
    let g3 = ident(Result::Ok(()));
    let g4: Result<(), string> = ident(Result::Err("e"));
    return format("%s %s %s %s", name_u(g1), name_t(g2), name_t(g4), name_u(g3));
}

fn main() {
    let o = stdout();
    let _ = write(o, to_bytes(format("%s\n", passing())));
    let _ = write(o, to_bytes(format("%s\n", equality())));
    let _ = write(o, to_bytes(format("%s\n", shows())));
    let _ = write(o, to_bytes(format("%s\n", stored())));
    let _ = write(o, to_bytes(format("%s\n", generic())));
}
