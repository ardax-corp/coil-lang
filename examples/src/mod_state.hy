// Helper module for tests/modules/module_state_*.hy: statics, consts, a
// class with private members and a trait, all used from another module.

const LIMIT = 10;

static const GREETING = "hello";

static let counter: int = 0;

static let names: Vec<string> = Vec::new();

fn bump() -> int {
    counter += 1;
    return counter;
}

fn count() -> int {
    return counter;
}

fn remember(string name) -> int {
    names.push(name);
    return names.len();
}

fn limit() -> int {
    return LIMIT;
}

class Account {
    pub owner: string,
    balance: int,
}

impl Account {
    pub static fn open(string owner) -> Account {
        return new Account(owner, 0);
    }

    pub fn deposit(int n) -> int {
        self.balance = self.clamp(self.balance + n);
        return self.balance;
    }

    pub fn balance() -> int {
        return self.balance;
    }

    fn clamp(int n) -> int {
        if n > LIMIT * 100 {
            return LIMIT * 100;
        }
        return n;
    }
}

trait Describe<T> {
    fn describe(T self) -> string {}

    fn loud(T self) -> string {
        return self.describe() + "!";
    }
}

impl Describe for Account {
    pub fn describe(Account self) -> string {
        return self.owner + ":" + self.balance().show();
    }
}
