// `old(e)` in an `ensures` reads `e` at entry: a method's field and a
// `Vec` argument's length; a broken one names its message and method.
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

class Account {
    pub balance: int,
}

impl Account {
    pub fn deposit(int amount)
        ensures self.balance == old(self.balance) + amount
    {
        self.balance = self.balance + amount;
    }

    pub fn skim(int amount)
        ensures self.balance == old(self.balance) + amount, "skimmed"
    {
        self.balance = self.balance + amount - 1;
    }
}

fn push_two(Vec<int> v) -> int
    ensures result == old(v.len()) + 2
{
    v.push(1);
    v.push(2);
    return v.len();
}

fn skim_three(Account a) -> int {
    a.skim(3);
    return 0;
}

fn main() {
    let a = new Account(10);
    a.deposit(5);
    let v: Vec<int> = Vec::new();
    if !(push_two(v) == 2) {
        panic "assertion failed";
    }
    expect(
        outcome(fn () use (a) => skim_three(a)),
        "contract violated: ensures self.balance == old(self.balance) + amount (\"skimmed\") in Account::skim",
    );
    write(stdout(), to_bytes("ok"));
}
