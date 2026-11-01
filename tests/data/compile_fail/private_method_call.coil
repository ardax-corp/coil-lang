// @expected E0128 — a method without `pub` is callable only inside its class's `impl`.
class Account {
    pub n: int,
}

impl Account {
    fn secret() -> int {
        return 1;
    }
}

fn main() {
    let a = new Account(1);
    let n = a.secret();
}
