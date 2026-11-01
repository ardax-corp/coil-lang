// #1561: another module's `const` table reads imported or by path, at a
// constant index (folded to the element) and a variable one, and a
// function of that module reads its own local table.
use tests::assert;
use const_tables::{PRIMES, prime_at, vowel};

fn sum_primes() -> int {
    let s = 0;
    for i in 0..PRIMES.len() {
        s += PRIMES[i];
    }
    return s;
}

test("an imported table") {
    assert!(PRIMES[0] == 2);
    assert!(PRIMES[5] == 13);
    assert!(PRIMES.len() == 6);
    assert!(sum_primes() == 41);
}

test("a table by its module path") {
    assert!(const_tables::PRIMES[2] == 5);
    let i = 3;
    assert!(const_tables::PRIMES[i] == 7);
}

test("the module's own reads") {
    assert!(prime_at(4) == 11);
    assert!(vowel(1) == "e");
    assert!(vowel(7) == "i");
}
