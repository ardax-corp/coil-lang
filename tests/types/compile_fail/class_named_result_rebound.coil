// @expected E0200 — `Result` is the built-in prelude enum; a class cannot
// take the name, even after `use prelude::Result as …` (#1672).
use prelude::Result as PreludeResult;

class Result {
    pub n: int,
}

fn get(Result r) -> int {
    return r.n;
}

fn main() {
    let _ = get(new Result(3));
}
