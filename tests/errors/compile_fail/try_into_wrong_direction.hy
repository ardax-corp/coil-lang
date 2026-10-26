// @expected E0102 — `?` converts only through `impl Into<FnErr> for Err`:
// an impl in the other direction does not let `Outer` flow into `Inner`.
enum Inner {
    A,
}

enum Outer {
    Wrap(Inner),
}

impl Into<Outer> for Inner {
    pub fn into(Inner e) -> Outer {
        return Outer::Wrap(e);
    }
}

fn outer() -> Result<int, Outer> {
    return Result::Ok(1);
}

fn inner() -> Result<int, Inner> {
    let v = outer()?;
    return Result::Ok(v);
}

fn main() {}
