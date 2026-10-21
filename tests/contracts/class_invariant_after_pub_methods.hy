// A class `invariant` holds after `new` and each `pub` method; a private
// helper may break it for a while. Failures name the method or `new C`.
// Output: ok
use io::{stdout, write};
use string::{to_bytes, find_from};
use task::{scope, Scope, TaskError};

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

// Panic unless `m` contains `part`.
fn expect(string m, string part) {
    if find_from(m, part, 0) < 0 {
        panic "expected `" + part + "` in: " + m;
    }
}

class Account
    invariant self.balance >= 0, "no overdraft"
{
    balance: int,
}

impl Account {
    pub static fn open(int start) -> Account {
        return new Account(start);
    }

    pub fn deposit(int amount) {
        self.balance = self.balance + amount;
    }

    pub fn withdraw(int amount) -> int {
        // A private helper may break the invariant for a while.
        self.take(amount + 1);
        self.balance = self.balance + 1;
        return self.balance;
    }

    fn take(int amount) {
        self.balance = self.balance - amount;
    }
}

fn open_negative() -> int {
    let _ = Account::open(-1);
    return 0;
}

fn main() {
    let a = Account::open(10);
    a.deposit(5);
    if !(a.withdraw(15) == 0) {
        panic "assertion failed";
    }
    expect(
        outcome(fn () use (a) => a.withdraw(1)),
        "contract violated: invariant self.balance >= 0 (\"no overdraft\") in Account::withdraw",
    );
    expect(outcome(fn () => open_negative()), "in new Account");
    let _ = write(stdout(), to_bytes("ok"));
}
