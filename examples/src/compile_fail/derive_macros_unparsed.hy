// @expected E0001 — a macro provider that does not parse: `pub` is not
// allowed on `trait`. tests/macros/compile_fail/macro_provider_parse_error.hy
// imports it.
use macro::{TypeDecl, Code, raw};

pub trait Broken

derive Named(TypeDecl t) -> Code {
    return raw("");
}

derive Twice(TypeDecl t) -> Code {
    return raw("");
}
