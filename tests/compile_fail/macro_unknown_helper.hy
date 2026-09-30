// Expected: compile failure — `#[json]` is not a helper of any derive on `C`.
use derive_macros::{FieldNames};

#[derive(FieldNames)]
class C {
    #[json(rename = "y")]
    pub x: int,
}
