// examples/nested_records.hy — nested record patterns (Phase 18B).
//
// The Phase 17B-cleanup pass documented the "nested record
// patterns" limitation: a record pattern inside an arm body
// was rejected because the codegen emitted a POP for the
// inner record instead of walking its declared fields.
//
// Phase 18B lifts the limitation. `Inner` and `Wrap` are
// both record-shaped enums; `Wrap::W { inner: Inner::I { v },
// name }` binds `v` and `name` from a single `match` — the
// inner record's `v` slot is reached by walking the inner
// record's declared fields in decl_order, with `emit_pattern_binding`
// recursing at unbounded depth.
//
// Output: `99` (the value of `v`).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Inner {
    I { v: int },
}

enum Wrap {
    W { inner: Inner, name: string },
}

fn get_v(Wrap w) -> int {
    return match w {
        Wrap::W{ inner: Inner::I{ v }, name } => v,
    };
}

fn main() {
    let w = Wrap::W{ inner: Inner::I{ v: 99 }, name: "x" };
    write_all(stdout(), to_bytes(format("%i", get_v(w))));
}
