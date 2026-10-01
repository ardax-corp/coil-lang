// Expected: E0119 — generated code does not parse.
use derive_macros_bad::{BadSyntax};

#[derive(BadSyntax)]
class C {
    pub x: int,
}
