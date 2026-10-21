// Expected: E0119 — the derive keeps more memory live than the cap.
// Flags: --comptime-mem 1
use derive_macros_bad::Hoard;

#[derive(Hoard)]
class C {
    pub x: int,
}
