// Fields and methods without `pub` are usable inside the class's own `impl`:
// private state is read, written and passed to private helpers there.
use tests::assert;

class Account {
    pub owner: string,
    balance: int,
    history: Vec<int>,
}

impl Account {
    pub static fn open(string owner) -> Account {
        return new Account(owner, 0, Vec::new());
    }

    pub fn deposit(int n) {
        self.balance = self.balance + self.after_fee(n);
        self.history.push(n);
    }

    pub fn balance_of() -> int {
        return self.balance;
    }

    pub fn moves() -> int {
        return self.history.len();
    }

    fn after_fee(int n) -> int {
        if n > 10 {
            return n - 1;
        }
        return n;
    }
}

test("private fields change through public methods") {
    let a = Account::open("ann");
    a.deposit(5);
    a.deposit(20);
    assert!(a.balance_of() == 24);
    assert!(a.moves() == 2);
    assert!(a.owner == "ann");
}

test("pub fields stay writable outside") {
    let a = Account::open("ann");
    a.owner = "bo";
    assert!(a.owner == "bo");
    assert!(a.balance_of() == 0);
}
