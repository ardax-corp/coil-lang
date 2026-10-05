// A user `into_iter` returning a numeric range, an array or a Vec runs
// that result's counted loop: the call, then the latch.
class Span {
    pub lo: int,
    pub hi: int,
}

impl IntoIterator for Span {
    type Item = int;
    type IntoIter = RangeInclusive<int>;
    fn into_iter(Span s) -> RangeInclusive<int> {
        return s.lo..=s.hi;
    }
}

class Bag {
    pub items: Vec<int>,
}

impl IntoIterator for Bag {
    type Item = int;
    type IntoIter = Vec<int>;
    fn into_iter(Bag b) -> Vec<int> {
        return b.items;
    }
}

fn span_sum(Span s) -> int {
    let acc = 0;
    for x in s {
        acc = acc + x;
    }
    return acc;
}

fn bag_max(Bag b) -> int {
    let best = 0;
    for x in b {
        if x > best {
            best = x;
        }
    }
    return best;
}

fn span_early(Span s, int stop) -> int {
    let n = 0;
    for x in s {
        if x == stop {
            break;
        }
        n = n + 1;
    }
    return n;
}

test("range-returning into_iter") {
    assert(span_sum(new Span(1, 4)) == 10)?;
    assert(span_sum(new Span(5, 4)) == 0)?;
    assert(span_early(new Span(0, 9), 3) == 3)?;
}

test("vec-returning into_iter") {
    let v = Vec::new();
    v.push(3);
    v.push(9);
    v.push(4);
    assert(bag_max(new Bag(v)) == 9)?;
}
