// Expected: E0128 — `balance` is private to `mod_state::Account`; importing the
// class does not open it.
use mod_state::Account;

fn main() {
    let a = Account::open("x");
    let _ = a.balance;
}
