// Expected: E0128 — `clamp` is a private method of `mod_state::Account`.
use mod_state::{Account};

fn main() {
    let a = Account::open("x");
    let _ = a.clamp(1);
}
