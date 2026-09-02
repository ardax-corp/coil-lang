// Canary (gate 1): hot match on boxed Option / Result / 3-variant payload
// enum. A later match/unbox cut should drop VM-only time vs this baseline.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

enum Phase {
    Low(int),
    Mid(int),
    High(int),
}

fn wrap_opt(int x) -> Option<int> {
    if x % 3 == 0 {
        return Option::None;
    }
    return Option::Some(x);
}

fn wrap_res(int x) -> Result<int, string> {
    if x % 5 == 0 {
        return Result::Err("miss");
    }
    return Result::Ok(x % 7);
}

fn wrap_phase(int x) -> Phase {
    let k = x % 3;
    if k == 0 {
        return Phase::Low(x);
    }
    if k == 1 {
        return Phase::Mid(x);
    }
    return Phase::High(x);
}

fn score_opt(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}

fn score_res(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn score_phase(Phase p) -> int {
    return match p {
        Phase::Low(v) => v,
        Phase::Mid(x) => x + 1,
        Phase::High(y) => y + 2,
    };
}

fn main() {
    let acc = 0;
    let i = 0;
    while i < 400000 {
        acc = acc + score_opt(wrap_opt(i));
        acc = acc + score_res(wrap_res(i));
        acc = acc + score_phase(wrap_phase(i));
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
