// A word popped above the cursor and exposed again by a cursor-raising
// store is not a must-pointer: a collection in between left it stale (#775).
use string::format;

fn word(int idx) -> string {
    if idx == 0 {
        return format("%s", "o");
    }
    return format("%s%s", "k", "");
}

fn count_hits(int n) -> int {
    let hits = 0;
    let i = 0;
    while i < n {
        if word(0) == "o" {
            if word(1) == "k" {
                hits = hits + 1;
            }
        }
        i = i + 1;
    }
    return hits;
}

test("string == on call results in a loop") {
    assert(count_hits(3) == 3)?;
}
