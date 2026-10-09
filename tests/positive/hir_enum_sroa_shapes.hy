// Local enums split into tag and field locals: string payloads, record
// variants, builtin enums, arms of variants no build makes, and a local
// built once whose match is decided at compile time.
enum Msg {
    Text(string),
    Code(int),
    Pair { lo: int, hi: int },
    Blob(int, int, int),
}

fn text(int i) -> string {
    let m = Msg::Code(i);
    if i > 2 {
        m = Msg::Text("big");
    }
    return match m {
        Msg::Text(s) => s,
        Msg::Code(c) => "code",
        Msg::Pair{ lo, hi } => "pair",
        Msg::Blob(a, b, c) => "blob",
    };
}

fn record(int i) -> int {
    let m = Msg::Pair{ lo: i, hi: i * 10 };
    if i == 0 {
        m = Msg::Code(7);
    }
    return match m {
        Msg::Pair{ lo, hi } => hi - lo,
        Msg::Code(c) => c,
        default => -1,
    };
}

fn shuffled(int i) -> int {
    let m = Msg::Pair{ hi: i * 10, lo: i };
    return match m {
        Msg::Pair{ lo, hi } => hi - lo,
        default => -1,
    };
}

fn fixed(int i) -> int {
    let m = Msg::Blob(i, i + 1, i + 2);
    return match m {
        Msg::Blob(a, b, c) => a + b * c,
        default => 0,
    };
}

fn maybe(int i) -> int {
    let o: Option<int> = Option::None;
    if i % 2 == 0 {
        o = Option::Some(i * 3);
    }
    return match o {
        Option::Some(v) => v,
        Option::None => -1,
    };
}

fn looped(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        let r: Result<int, string> = Result::Ok(i);
        if i % 3 == 0 {
            r = Result::Err("skip");
        }
        total = total + match r {
            Result::Ok(v) => v,
            Result::Err(e) => len(e),
        };
        i = i + 1;
    }
    return total;
}

test("string payloads and unbuilt arms") {
    assert(text(1) == "code")?;
    assert(text(5) == "big")?;
}

test("record variants in any field order") {
    assert(record(3) == 27)?;
    assert(record(0) == 7)?;
    assert(shuffled(2) == 18)?;
}

test("a local built once") {
    assert(fixed(2) == 14)?;
}

test("builtin enums") {
    assert(maybe(4) == 12)?;
    assert(maybe(3) == -1)?;
    assert(looped(6) == 20)?;
}
