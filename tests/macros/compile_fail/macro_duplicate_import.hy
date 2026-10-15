// Expected: E0119 — `FieldNames` is imported from two modules.
use derive_macros::{FieldNames};
use derive_macros_bad::{FieldNames};

#[derive(FieldNames)]
class C {
    pub x: int,
}
