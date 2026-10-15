// Direct MIR reads record keys and a `#[repr(string)]` variant's backing
// string by their interned index, which the table walk interns as the
// body's MIR is planned: a record's keys, and a string-backed variant a
// body only matches.
use tests::assert;

#[repr(string)]
enum Tone {
    Low = "lo",
    High = "hi",
}

fn is_high(Tone t) -> bool {
    return match t {
        Tone::High => true,
        default => false,
    };
}

fn high() -> Tone {
    return Tone::High;
}

fn sum_fields(int a, int b) -> int {
    let p = { left: a, right: b };
    p.right += 1;
    return p.left + p.right;
}

test("matched variant") {
    assert!(is_high(high()));
    assert!(!is_high(Tone::Low));
}

test("record keys") {
    assert!(sum_fields(3, 4) == 8);
}
