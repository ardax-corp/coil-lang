// Built-in Option / Result, ?, raise, ??, ?..
use tests::assert;

fn parse_pos(int n, int is_neg) {
    if is_neg == 1 {
        raise "neg";
    }
    return n;
}

fn double_pos(int n, int is_neg) {
    let v = parse_pos(n, is_neg)?;
    return v * 2;
}

fn show_i(Result r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn show_e(Result r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn opt_inc(Option o) -> int {
    return match o {
        Option::Some(n) => n + 1,
        Option::None => 0,
    };
}

test("result ok path") {
    assert!(show_i(double_pos(5, 0)) == 10);
}

test("result err path via raise") {
    assert!(show_e(double_pos(1, 1)) == "neg");
    assert!(show_i(double_pos(1, 1)) == -1);
}

test("option some and none") {
    assert!(opt_inc(Option::Some(41)) == 42);
    assert!(opt_inc(Option::None) == 0);
}

test("coalesce option") {
    assert!((Option::None ?? "bar") == "bar");
    assert!((Option::Some("hi") ?? "bar") == "hi");
}

test("coalesce result swallows err") {
    assert!((Result::Err("boom") ?? 7) == 7);
    assert!((Result::Ok(9) ?? 7) == 9);
}

test("optional chain on dict") {
    let some = Option::Some({ v: 42 });
    let none = Option::None;
    let a = match some?.v {
        Option::Some(n) => n,
        Option::None => 0,
    };
    let b = none?.v ?? 0;
    assert!(a == 42);
    assert!(b == 0);
}

fn fails() -> Result<(), string> {
    assert!(false);
    return Result::Ok(());
}

fn fails_with(string message) -> Result<(), string> {
    assert!(1 == 0, message);
    return Result::Ok(());
}

test("assert soft-fail returns Err") {
    let r = fails();
    let ok = match r {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
    assert!(ok);
}

test("assert custom message") {
    let msg = match fails_with("custom") {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    assert!(msg == "custom");
}
