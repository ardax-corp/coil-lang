// A module's statics are one variable for the whole program, whichever
// module reads or writes them; its functions, consts, classes
// (constructors, static and instance methods) and trait impls are usable from
// the importer, by name or by qualified path.
use tests::assert;
use mod_state::{bump, count, remember, limit, counter, GREETING, Account, Describe};

test("a module static changes through its functions") {
    assert!(count() == 0);
    assert!(bump() == 1);
    assert!(bump() == 2);
    assert!(count() == 2);
}

test("a heap static in another module grows") {
    assert!(remember("a") == 1);
    assert!(remember("b") == 2);
}

test("a module's const is read through its function") {
    assert!(limit() == 10);
}

test("an imported class with private state behind pub methods") {
    let a = Account::open("ada");
    assert!(a.owner == "ada");
    assert!(a.deposit(5) == 5);
    assert!(a.deposit(2000) == 1000);
    assert!(a.balance() == 1000);
    let b = Account::open("cy");
    assert!(b.deposit(1000) == 1000);
    assert!(b.deposit(5) == 1000);
}

test("an imported trait's methods, including its default") {
    let a = Account::open("bo");
    a.deposit(3);
    assert!(a.describe() == "bo:3");
    assert!(a.loud() == "bo:3!");
}

test("an imported static is the module's own variable") {
    assert!(counter == 0);
    counter = 40;
    assert!(count() == 40);
    bump();
    assert!(counter == 41);
    counter += 1;
    assert!(count() == 42);
}

test("an imported string const") {
    assert!(GREETING == "hello");
}

test("a module function and static by qualified path") {
    assert!(mod_state::bump() == 1);
    assert!(mod_state::counter == 1);
    assert!(mod_state::count() == 1);
}
