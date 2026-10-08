// `==` on strings returned by a call inside a loop compares content, not the
// heap word. The dense loop path used to compare the two pointers.
use string::format;

fn word(int idx) -> string {
    let parts: Vec<string> = Vec::new();
    parts.push("o");
    parts.push("k");
    return format("%s", parts[idx]);
}

fn count_hits(int n) -> int {
    let hits = 0;
    let i = 0;
    while i < n {
        let a = word(i % 2) == "o";
        let b = word(1 - i % 2) == "k";
        if a && b {
            hits = hits + 1;
        }
        i = i + 1;
    }
    return hits;
}

fn pick(Vec<string> rows, string key) -> string {
    let v = "";
    let i = 0;
    while i < len(rows) {
        if word(0) == "o" && format("%s", rows[i]) == key {
            v = rows[i];
        }
        i = i + 1;
    }
    return v;
}

test("string == on call results in a loop") {
    assert(count_hits(4) == 2)?;
}

test("string == in a loop condition") {
    let rows: Vec<string> = Vec::new();
    rows.push("a");
    rows.push("k");
    assert(pick(rows, "k") == "k")?;
}
