// @expected E0102 — `join` on a `Task<int>` gives `Result<int, TaskError>`;
// its `Ok` value is not a `string`.
use task::{scope, Scope};

fn main() {
    let r = scope(fn (Scope s) {
        let t = s.spawn(fn () => 1);
        let v: string = match t.join() {
            Result::Ok(n) => n,
            Result::Err(_) => "x",
        };
        0
    });
}
