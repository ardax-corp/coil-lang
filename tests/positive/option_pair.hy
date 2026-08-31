class Holder {
    pub text: string,
}

fn optional_text(Option<Holder> value) -> Option<string> {
    return value?.text;
}

fn parse_pair(int n, int fail) {
    if fail == 1 {
        raise "bad";
    }
    return n;
}

fn match_pair(int fail) {
    return match parse_pair(7, fail) {
        Result::Ok(value) => value,
        Result::Err(_) => -1
};
}

fn chain_pair(int fail) {
    let value = parse_pair(7, fail)?;
    return value + 1;
}

fn pass_value<T>(T value) -> T {
    return value;
}

fn generic_option(Option value) -> string {
    return pass_value(value) ?? "none";
}

fn indirect_pair() -> int {
    let function = parse_pair;
    return match function(7, 0) {
        Result::Ok(value) => value,
        Result::Err(_) => -1
};
}

test("direct pair match") {
    assert(match_pair(0) == 7)?;
    assert(match_pair(1) == -1)?;
}

test("pair try propagation") {
    assert(
        match chain_pair(0) {
            Result::Ok(value) => value == 8,
            Result::Err(_) => false
},
    )?;
    assert(match chain_pair(1) {
        Result::Ok(_) => false,
        Result::Err(_) => true
})?;
}

test("pointer niche option from Vec pop") {
    let values = Vec::from(["a", "b"],);
    let last = match values.pop() {
        Option::Some(value) => value,
        Option::None => "none"
};
    assert(last == "b")?;
    let _ = values.pop();
    let empty = match values.pop() {
        Option::Some(_) => false,
        Option::None => true
};
    assert(empty)?;
}

test("generic Option boundary") {
    assert(generic_option(Option::Some("ok",)) == "ok")?;
    assert(generic_option(Option::None) == "none")?;
}

test("indirect Result boundary") {
    assert(indirect_pair() == 7)?;
}

test("pointer niche optional access") {
    let some = match optional_text(Option::Some(new Holder("value"),)) {
        Option::Some(text) => text,
        Option::None => "none"
};
    let none = match optional_text(Option::None) {
        Option::Some(_) => "bad",
        Option::None => "none"
};
    assert(some == "value")?;
    assert(none == "none")?;
}
