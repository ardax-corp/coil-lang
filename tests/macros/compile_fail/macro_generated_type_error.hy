// Expected: E0102 — generated code has a type error, reported at
// the `#[derive]`.
use derive_macros_bad::{BadType};

#[derive(BadType)]
class C {
    pub x: int,
}
