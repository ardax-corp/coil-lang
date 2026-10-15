// Methods take `requires` / `ensures` like functions, `ensures` can read
// `self`, and `old(e)` in `ensures` is `e` as it was on entry, evaluated
// once. A unit body that runs off its end is checked there.
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

class Account {
    pub balance: int,
    pub ops: int,
}

impl Account {
    pub fn deposit(int amount)
        requires amount > 0, "deposit must be positive"
        ensures self.balance == old(self.balance) + amount
        ensures self.ops == old(self.ops) + 1
    {
        self.balance = self.balance + amount;
        self.ops = self.ops + 1;
    }

    // Says it adds `amount` but adds one more.
    pub fn sloppy_deposit(int amount)
        ensures self.balance == old(self.balance) + amount
    {
        self.balance = self.balance + amount + 1;
    }

    pub fn withdraw(int amount) -> int
        requires amount <= self.balance
        ensures result == old(self.balance) - amount
        ensures self.balance == result
    {
        self.balance = self.balance - amount;
        return self.balance;
    }
}

class Cell {
    pub v: int,
}

// `old` of a parameter's field after the body overwrote it.
fn swap_in(Cell c, int v) -> int
    ensures result == old(c.v)
    ensures c.v == v
{
    let prev = c.v;
    c.v = v;
    return prev;
}

test("method clauses hold for a correct method") {
    let a = new Account(10, 0);
    a.deposit(5);
    a.deposit(1);
    assert!(a.balance == 16);
    assert!(a.ops == 2);
    assert!(a.withdraw(6) == 10);
}

test("a method's requires blames the caller") {
    let m = outcome(
        fn () {
            let a = new Account(0, 0);
            a.deposit(0);
            0
        },
    );
    assert!(has(m, "contract violated: requires amount > 0 (\"deposit must be positive\")"), m);
    assert!(has(m, "called from"), m);
}

test("a requires can read the receiver") {
    let m = outcome(fn () => new Account(3, 0).withdraw(4));
    assert!(has(m, "requires amount <= self.balance"), m);
}

test("old is the value on entry") {
    let c = new Cell(1);
    assert!(swap_in(c, 9) == 1);
    assert!(c.v == 9);
    assert!(swap_in(c, 4) == 9);
}

test("a unit method that breaks its ensures panics at its end") {
    let m = outcome(
        fn () {
            let a = new Account(2, 0);
            a.sloppy_deposit(3);
            0
        },
    );
    assert!(has(m, "ensures self.balance == old(self.balance) + amount"), m);
    assert!(!has(m, "called from"), m);
}
