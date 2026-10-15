// `case` is not a keyword (COI-74): it names a local, and a bare `case`
// pattern in a `match` arm is a binding.
use tests::assert;

test("case as a local and a match binding") {
    let case = 1;
    let r = match case {
        case => case + 1,
    };
    assert!(r == 2);
}
