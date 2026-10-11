// `format` calls in bodies lowered straight from HIR to MIR: the format
// string loads first, then the arguments, then one Format instruction.
use string::format;

fn show(int n) -> string {
    return format("n=%i", n);
}

fn mixed(int n, float x, string s) -> string {
    return format("n=%i x=%f s=%s %%", n, x, s);
}

fn tally(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        total = total + len(format("%i", i));
        i = i + 1;
    }
    return total;
}

test("format in direct MIR") {
    assert(show(42) == "n=42")?;
    assert(len(mixed(1, 2.5, "a")) > 0)?;
    assert(tally(12) == 14)?;
}
