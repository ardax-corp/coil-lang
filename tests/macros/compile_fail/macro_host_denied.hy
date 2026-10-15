// Expected: E0119 — macros cannot read the environment.
use derive_macros_bad::{Peek};

#[derive(Peek)]
class C {
    pub x: int,
}
