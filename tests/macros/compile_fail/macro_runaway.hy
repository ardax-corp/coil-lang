// @expected E0119 — the derive loops forever (step budget).
// @flags --comptime-steps 1000000
use derive_macros_bad::Spin;

#[derive(Spin)]
class C {
    pub x: int,
}
