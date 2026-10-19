// A bare unit case of an IMPORTED enum (`On`, `Off`) in a pure recursive
// function is a refutable constructor pattern for the auto-par pass too, as
// it is for the checker: the fork inside `On` runs only when `On` matches.
use tests::assert;
use bare_case_provider::Switch;
use bare_case_provider::mode;

fn fibs(int n) -> int {
    return match mode(n) {
        On => fibs(n - 1) + fibs(n - 2),
        Off => n,
    };
}

test("bare imported unit case arms") {
    assert!(fibs(1) == 1);
    assert!(fibs(20) == 6765);
    assert!(fibs(30) == 832040);
}
