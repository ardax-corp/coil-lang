// #1831: a local whose `let` binds a literal (or `None`) and which is never
// read, but is assigned a call's value (in a loop or straight-line),
// compiles in direct MIR: the dropped `let` used to leave the assignments
// with no slot. Each assigned value is still computed, for its effects.
use tests::assert;
use string::format;

class Box {
    pub v: int,
}

static let calls: int = 0;

fn text(int i) -> string {
    calls = calls + 1;
    return format("%i", i);
}

fn number(int i) -> int {
    calls = calls + 1;
    return i * 7;
}

fn boxed(int i) -> Box {
    calls = calls + 1;
    return new Box(i);
}

fn list(int i) -> Vec<int> {
    calls = calls + 1;
    let v: Vec<int> = Vec::new();
    v.push(i);
    return v;
}

fn string_in_for() {
    let s = "";
    for i in 0..3 {
        s = format("%i", i);
    }
}

fn string_call_in_for() {
    let s = "";
    for i in 0..3 {
        s = text(i);
    }
}

fn string_in_while() {
    let s = "";
    let i = 0;
    while i < 3 {
        s = text(i);
        i += 1;
    }
}

fn string_straight() {
    let s = "";
    s = text(1);
}

fn int_in_for() {
    let n = 0;
    for i in 0..3 {
        n = number(i);
    }
}

fn option_in_for() {
    let o: Option<string> = None;
    for i in 0..3 {
        o = Some(text(i));
    }
}

fn class_in_for() {
    let b = new Box(0);
    for i in 0..3 {
        b = boxed(i);
    }
}

fn class_type_only_in_for() {
    let b: Box;
    for i in 0..3 {
        b = boxed(i);
    }
}

fn vec_in_for() {
    let v: Vec<int> = Vec::new();
    for i in 0..3 {
        v = list(i);
    }
}

fn vec_type_only_in_for() {
    let v: Vec<int>;
    for i in 0..3 {
        v = list(i);
    }
}

fn main() {
    string_in_for();
    string_call_in_for();
    assert!(calls == 3);
    string_in_while();
    assert!(calls == 6);
    string_straight();
    assert!(calls == 7);
    int_in_for();
    assert!(calls == 10);
    option_in_for();
    assert!(calls == 13);
    class_in_for();
    assert!(calls == 16);
    class_type_only_in_for();
    assert!(calls == 19);
    vec_in_for();
    assert!(calls == 22);
    vec_type_only_in_for();
    assert!(calls == 25);
}
