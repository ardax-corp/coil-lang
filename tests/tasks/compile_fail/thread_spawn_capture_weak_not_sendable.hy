// Expected: E0119 — a spawned closure's `use` captures are copied to the
// thread like its argument, so a captured `gc::Weak` is not sendable (#1199).
use thread::{spawn, join};
use gc::{weak, Weak};

fn main() {
    let w = weak("x");
    let t = spawn(fn () use (w) => 1)?;
    join(t)?;
}
