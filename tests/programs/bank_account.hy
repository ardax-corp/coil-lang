// An account class whose methods raise and `?` through `Result<unit, string>`, with a `Vec` of enum transactions folded back into a total.
// @output ok 150 err:insufficient funds 150 2 [Tx::Deposit(100), Tx::Deposit(50)] total=150\n
use io::sync::println;
use string::format;

#[derive(Show, Eq)]
enum Tx {
    Deposit(int),
    Withdraw(int),
}

class Account {
    balance: int,
    history: Vec<Tx>,
}

impl Account {
    pub static fn open() -> Account {
        return new Account(0, Vec::new());
    }
    pub fn deposit(int amt) -> Result<unit, string> {
        if amt <= 0 {
            raise "bad amount";
        }
        self.balance += amt;
        self.history.push(Tx::Deposit(amt));
    }
    pub fn withdraw(int amt) -> Result<unit, string> {
        if amt > self.balance {
            raise "insufficient funds";
        }
        self.balance -= amt;
        self.history.push(Tx::Withdraw(amt));
    }
    pub fn balance() -> int {
        return self.balance;
    }
    pub fn history() -> Vec<Tx> {
        return self.history;
    }
    pub fn total_in() -> int {
        let t = 0;
        for tx in self.history {
            match tx {
                Tx::Deposit(a) => {
                    t += a;
                },
                Tx::Withdraw(_) => {},
            }
        }
        return t;
    }
}

fn run(Account acc) -> Result<string, string> {
    acc.deposit(100)?;
    acc.deposit(50)?;
    return "ok";
}

fn main() {
    let acc = Account::open();
    let r = match run(acc) {
        Result::Ok(s) => s,
        Result::Err(e) => "err:" + e,
    };
    let w = match acc.withdraw(500) {
        Result::Ok(_) => "ok",
        Result::Err(e) => "err:" + e,
    };
    let _ = println(
        format(
            "%s %i %s %i %i %v total=%i",
            r,
            acc.balance(),
            w,
            acc.balance(),
            acc.history().len(),
            acc.history(),
            acc.total_in(),
        ),
    );
}
