class TextCounter {
    pub cur: int,
    pub end: int,
    pub text: string,
}

impl IntoIterator for TextCounter {
    type Item = string;
    type IntoIter = TextCounter;
    fn into_iter(TextCounter value) -> TextCounter {
        return value;
    }
}

impl Iterator for TextCounter {
    type Item = string;
    fn next(TextCounter value) -> Option<string> {
        if value.cur < value.end {
            value.cur = value.cur + 1;
            return Option::Some(value.text);
        }
        return Option::None;
    }
}

test("iterator yielding Option<string> (pointer niche)") {
    let joined = "";
    let n = 0;
    for text in new TextCounter(0, 3, "ab") {
        joined = joined + text;
        n = n + 1;
    }
    assert(n == 3)?;
    assert(joined == "ababab")?;
}

test("empty niche iterator") {
    let n = 0;
    for text in new TextCounter(2, 2, "x") {
        n = n + 1;
    }
    assert(n == 0)?;
}
