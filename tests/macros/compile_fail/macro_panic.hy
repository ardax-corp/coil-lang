// Expected: E0119 — the derive panics with a message.
use derive_macros_bad::{Refuse};

#[derive(Refuse)]
class C {
    pub x: int,
}
