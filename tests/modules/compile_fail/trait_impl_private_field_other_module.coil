// @expected E0128 — a trait impl outside the class's own module does not open
// its private fields; only the defining module's impls do (#2084).
use mod_state::Account;

trait Peek<T> {
    fn peek(T self) -> int {}
}

impl Peek for Account {
    pub fn peek(Account a) -> int {
        return a.balance;
    }
}

fn main() {
    let _ = Account::open("x").peek();
}
