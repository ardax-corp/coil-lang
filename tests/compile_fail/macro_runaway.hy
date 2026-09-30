// Expected: compile failure — the derive loops forever (step budget).
use derive_macros_bad::{Spin};

#[derive(Spin)]
class C {
    pub x: int,
}
