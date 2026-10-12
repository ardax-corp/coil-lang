// String `==` and `!=` compare content, whether the result feeds a branch,
// a local or the return value, on fields, parameters and fresh strings.
use string::format;

class Slot {
    pub on: int,
    pub key: string,
}

fn same(string a, string b) -> bool {
    return a == b;
}

fn differ(string a, string b) -> bool {
    let d = a != b;
    return d;
}

fn match_key(Slot slot, string key) -> int {
    if slot.on == 0 {
        return 0;
    }
    if slot.key != key {
        return 2;
    }
    return 1;
}

fn count_eq(Vec<string> xs, string key) -> int {
    let n = 0;
    let i = 0;
    while i < len(xs) {
        let hit = xs[i] == key;
        if hit {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

test("string == compares content") {
    let a = format("%s%s", "o", "k");
    assert(same(a, "ok"))?;
    assert(!same(a, "no"))?;
    assert(!differ(a, "ok"))?;
    assert(differ(a, "ko"))?;
}

test("string != on a field") {
    let s = new Slot(1, format("%s", "key"));
    assert(match_key(s, "key") == 1)?;
    assert(match_key(s, "other") == 2)?;
    assert(match_key(new Slot(0, "key"), "key") == 0)?;
}

test("string == stored in a loop") {
    let xs: Vec<string> = Vec::new();
    xs.push(format("%s", "a"));
    xs.push("b");
    xs.push(format("%s%s", "", "a"));
    assert(count_eq(xs, "a") == 2)?;
    assert(count_eq(xs, "c") == 0)?;
}
