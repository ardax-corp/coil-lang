// `.member` takes leading whitespace and line breaks after every receiver
// (#1540): a literal, a `.field`, a postfix `?`, an identifier or a call.
// Floats (`1.5`) and ranges (`..`, `..=`) keep their meaning next to it.
use tests::assert;

class Holder {
    pub text: string,
    pub inner: Inner,
}

class Inner {
    pub n: int,
}

fn holder() -> Holder {
    return new Holder("abcd", new Inner(7));
}

fn text_len(Holder h) -> Result<int, string> {
    let r: Result<string, string> = Result::Ok(h.text);
    return Result::Ok(r? .len());
}

fn text_len_broken(Holder h) -> Result<int, string> {
    let r: Result<string, string> = Result::Ok(h.text);
    return Result::Ok(r?
        .len());
}

fn optional_text(Option<Holder> value) -> Option<string> {
    return value ?.text;
}

test("string literal receiver") {
    let a = "abc"
        .len();
    let b = "abc" .len();
    assert!(a == 3);
    assert!(b == 3);
}

test("number literal receiver") {
    let a = 12
        .show();
    let b = 12 .show();
    let c = 2.5
        .show();
    assert!(a == "12", a);
    assert!(b == "12", b);
    assert!(c == "2.5", c);
}

test("field receiver") {
    let h = holder();
    let a = h.text
        .len();
    let b = h.inner .n;
    let c = h
        .inner
        .n
        .show();
    assert!(a == 4);
    assert!(b == 7);
    assert!(c == "7", c);
}

test("try receiver") {
    let h = holder();
    assert!(text_len(h) == Result::Ok(4));
    assert!(text_len_broken(h) == Result::Ok(4));
    assert!(optional_text(Option::Some(h)) == Option::Some("abcd"));
}

test("floats and ranges are unchanged") {
    let f = 1.5;
    assert!(f > 1.0);
    let total = 0;
    for i in 0..3 {
        total += i;
    }
    for i in 0 .. 3 {
        total += i;
    }
    for i in 1..=2 {
        total += i;
    }
    let n = 2;
    for i in n ..= 3 {
        total += i;
    }
    assert!(total == 14);
}
