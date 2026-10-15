// Expected: E0128 — a field without `pub` is writable only inside its class's `impl`.
class Account {
    balance: int,
}

fn main() {
    let a = new Account(1);
    a.balance = 2;
}
