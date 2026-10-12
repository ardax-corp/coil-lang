// A call whose `Option` comes back boxed (`Vec::pop`'s thunk) in bodies
// lowered straight from HIR to MIR: dropped as it comes back, or matched
// boxed with a `JumpIfMatch`.
use string::format;

fn drop_all(int n) -> int {
    let xs: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        xs.push(i);
        i = i + 1;
    }
    let _ = xs.pop();
    xs.pop();
    return xs.len();
}

fn drain(int n) -> int {
    let xs: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        xs.push(i * 2);
        i = i + 1;
    }
    let total = 0;
    let more = true;
    while more {
        match xs.pop() {
            Option::Some(x) => {
                total = total + x;
            },
            Option::None => {
                more = false;
            },
        }
    }
    return total;
}

test("boxed Option results in direct MIR") {
    assert(drop_all(5) == 3)?;
    assert(drop_all(1) == 0)?;
    assert(drain(4) == 12)?;
    assert(drain(0) == 0)?;
}

enum Tok {
    Word(string),
    Num(string),
}

fn width(Tok t) -> int {
    return match t {
        Tok::Word(s) => s.len(),
        Tok::Num(s) => 100 + s.len(),
    };
}

fn widths(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        let t = Tok::Word(format("w%i", i));
        if i % 3 == 0 {
            t = Tok::Num(format("%i", i));
        }
        total = total + width(t);
        i = i + 1;
    }
    return total;
}

test("a heap payload pair boxed for a call") {
    assert(widths(7) == 311)?;
}
