// A class `invariant` is checked after `new C(…)` and when a `pub` method
// returns. A private helper may break it for a while, static methods are
// not checked, and a failure names the method or `new C`.
use tests::assert;
use task::{scope, Scope, TaskError};
use string::find_from;

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task; its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

fn has(string m, string part) -> bool {
    return find_from(m, part, 0) >= 0;
}

class Account
    invariant self.balance >= 0, "no overdraft"
    invariant self.limit > 0
{
    balance: int,
    limit: int,
}

impl Account {
    pub static fn open(int limit) -> Account {
        return new Account(0, limit);
    }

    // A static method has no receiver to check.
    pub static fn raw(int balance, int limit) -> int {
        return balance + limit;
    }

    pub fn balance() -> int {
        return self.balance;
    }

    pub fn withdraw(int amount) {
        self.balance = self.balance - amount;
    }

    // Goes through a negative balance in a private helper, then repairs it.
    pub fn transfer(int out, int back) {
        self.take(out);
        self.give(back);
    }

    fn take(int n) {
        self.balance = self.balance - n;
    }

    fn give(int n) {
        self.balance = self.balance + n;
    }
}

test("construction and methods that keep the invariant") {
    let a = new Account(5, 10);
    a.withdraw(5);
    assert!(a.balance() == 0);
    assert!(Account::open(3).balance() == 0);
    assert!(Account::raw(0 - 1, 3) == 2);
}

test("a private helper may break the invariant in the middle") {
    let a = new Account(2, 10);
    a.transfer(5, 4);
    assert!(a.balance() == 1);
}

test("construction that breaks the invariant names new") {
    let m = outcome(fn () => new Account(0 - 1, 10).balance());
    assert!(
        has(m, "contract violated: invariant self.balance >= 0 (\"no overdraft\") in new Account"),
        m,
    );
}

test("each invariant clause is checked") {
    let m = outcome(fn () => new Account(1, 0).balance());
    assert!(has(m, "invariant self.limit > 0 in new Account"), m);
}

test("a pub method that breaks the invariant names the method") {
    let m = outcome(fn () {
        let a = new Account(1, 10);
        a.withdraw(2);
        0
    });
    assert!(has(m, "invariant self.balance >= 0"), m);
    assert!(has(m, "in Account::withdraw"), m);
}

test("a pub method that leaves a private helper's damage panics") {
    let m = outcome(fn () {
        let a = new Account(1, 10);
        a.transfer(5, 1);
        0
    });
    assert!(has(m, "in Account::transfer"), m);
}
