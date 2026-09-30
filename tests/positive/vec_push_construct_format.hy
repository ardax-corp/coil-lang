// COI-16: Construct with nested format must stage the Vec::push receiver.
// (Nested format-inside-Construct field values are a separate open clobber.)
use string::format;

enum Row {
    Pair(string, string),
}

test("push enum construct with format args keeps len") {
    let rows = Vec::new();
    rows.push(Row::Pair(format("a=%s", "1"), format("b=%s", "2")));
    rows.push(Row::Pair(format("c=%s", "3"), format("d=%s", "4")));
    assert(len(rows) == 2)?;
}
