// Helper module for tests/modules/const_table_import.hy: a module `const`
// table and a function with a local one, used from another module (#1561).

const PRIMES = readonly [2, 3, 5, 7, 11, 13];

fn prime_at(int i) -> int {
    return PRIMES[i];
}

fn vowel(int i) -> string {
    const VOWELS = readonly ["a", "e", "i", "o", "u"];
    return VOWELS[i % 5];
}
