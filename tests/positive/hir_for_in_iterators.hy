// For-in over a tuple walks its elements; over a user iterator it calls
// `into_iter` once, then `next` until `None`. Items come back as two words
// (`Option<int>`), as a niche word (`Option<string>`) or boxed.
class Counter {
    pub cur: int,
    pub end: int,
}

impl IntoIterator for Counter {
    type Item = int;
    type IntoIter = Counter;
    pub fn into_iter(Counter c) -> Counter {
        return c;
    }
}

impl Iterator for Counter {
    type Item = int;
    pub fn next(Counter c) -> Option<int> {
        if c.cur < c.end {
            let v = c.cur;
            c.cur = c.cur + 1;
            return Option::Some(v);
        }
        return Option::None;
    }
}

class Words {
    pub n: int,
}

impl IntoIterator for Words {
    type Item = string;
    type IntoIter = Words;
    pub fn into_iter(Words w) -> Words {
        return w;
    }
}

impl Iterator for Words {
    type Item = string;
    pub fn next(Words w) -> Option<string> {
        if w.n > 0 {
            w.n = w.n - 1;
            return Option::Some("ab");
        }
        return Option::None;
    }
}

fn count_sum(int n) -> int {
    let s = 0;
    for x in new Counter(0, n) {
        if x == 2 {
            continue;
        }
        if x == 6 {
            break;
        }
        s += x;
    }
    return s;
}

fn word_chars(int n) -> int {
    let total = 0;
    for w in new Words(n) {
        total += len(w);
    }
    return total;
}

fn tuple_sum() -> int {
    let s = 0;
    for v in (3, 4, 5) {
        if v == 5 {
            break;
        }
        s += v;
    }
    return s;
}

test("int iterator with continue and break") {
    assert(count_sum(4) == 4)?;
    assert(count_sum(10) == 13)?;
    assert(count_sum(0) == 0)?;
}

test("niche string iterator") {
    assert(word_chars(3) == 6)?;
    assert(word_chars(0) == 0)?;
}

test("tuple elements") {
    assert(tuple_sum() == 7)?;
}
