// A last match arm that binds only some payload fields still unpacks the
// whole variant: the unused fields' payload reads must not be dropped.
use tests::assert;

enum Trio {
    Three(string, string, string),
}

enum Five {
    Five(int, int, int, int, int),
}

test("an unused middle field keeps the last field in place") {
    let t = Trio::Three("a", "b", "c");
    let got = match t {
        Trio::Three(x, y, z) => x + z,
    };
    assert!(got == "ac");
}

test("unused trailing fields still unpack") {
    let f = Five::Five(1, 2, 3, 4, 5);
    let first = match f {
        Five::Five(
            a,
            b,
            c,
            d,
            e,
        ) => a,
    };
    assert!(first == 1);
}
