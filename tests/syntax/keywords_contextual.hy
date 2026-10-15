// Contextual words are keywords only in their position: `derive`, `attrs`,
// `macro`, `quote`, `gen`, `pure`, `uses`, `requires`, `ensures`,
// `invariant` and `decreases` are ordinary names elsewhere.
use tests::assert;

fn uses(int pure) -> int {
    return pure + 1;
}

test("contextual words as local names") {
    let derive = 1;
    let attrs = 2;
    let macro = 3;
    let quote = 4;
    let gen = 5;
    assert!(derive + attrs + macro + quote + gen == 15);
    let requires = 6;
    let ensures = 7;
    let invariant = 8;
    let decreases = 9;
    assert!(requires + ensures + invariant + decreases == 30);
}

test("contextual words as function and parameter names") {
    assert!(uses(41) == 42);
}

test("contextual words as record fields") {
    let d = { pure: 1, gen: 2 };
    assert!(d.pure + d.gen == 3);
}
