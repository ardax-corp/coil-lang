// @expected E0119 — a field attribute no derive owns is still unknown when an
// attribute macro re-emits the type (#1846).
use derive_macros::with_describe;

#[with_describe(prefix = "p")]
class P {
    #[bogus(name = "x")]
    pub id: int,
}

fn main() {}
