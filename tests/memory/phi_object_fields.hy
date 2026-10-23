// @output 48 -4 | 6 54 -1 | 56 -4 | -14 | -1:0,1:1,2:2,-1:0,4:4,
// An object made on each path into a join (`locate(i) ?? new Cut(-1,
// -1)`) and only read field by field after it needs no allocation: its
// fields meet as values. One that escapes (kept in a `Vec`), is written
// after the join, or is a shared default stays an object, and every
// form reads the same fields.
use io::{stdout, write};
use string::to_bytes;

class Cut {
    pub lo: int,
    pub hi: int,
}

fn locate(int x) -> Option<Cut> {
    if x % 3 == 0 {
        return Option::None;
    }
    return Option::Some(new Cut(x, x * 2));
}

fn spans(int n) -> string {
    let total = 0;
    let misses = 0;
    let i = 0;
    while i < n {
        let r = locate(i) ?? new Cut(-1, -1);
        if r.lo >= 0 {
            total = total + r.hi - r.lo;
        } else {
            misses = misses - 1;
        }
        i = i + 1;
    }
    return show(total) + " " + show(misses);
}

fn kept(int n) -> string {
    let all: Vec<Cut> = Vec::new();
    let i = 0;
    while i < n {
        let r = locate(i) ?? new Cut(-1, -1);
        all.push(r);
        i = i + 1;
    }
    let hits = 0;
    let sum = 0;
    let miss = 0;
    for r in all {
        if r.lo >= 0 {
            hits = hits + 1;
            sum = sum + r.hi;
        } else {
            miss = miss - 1;
        }
    }
    return show(hits) + " " + show(sum + miss * 0) + " " + show(miss / 3);
}

fn written(int n) -> string {
    let total = 0;
    let misses = 0;
    let i = 0;
    while i < n {
        let r = locate(i) ?? new Cut(-1, -1);
        r.hi = r.hi + 1;
        if r.lo >= 0 {
            total = total + r.hi - r.lo;
        } else {
            misses = misses - 1;
        }
        i = i + 1;
    }
    return show(total) + " " + show(misses);
}

fn same(int n) -> int {
    let fallback = new Cut(-1, -1);
    let total = 0;
    let i = 0;
    while i < n {
        let r = locate(i) ?? fallback;
        if r.lo < 0 {
            // The shared default: writing it shows in the next miss.
            fallback.hi = fallback.hi - 1;
            total = total + r.hi;
        }
        i = i + 1;
    }
    return total;
}

fn labels(int n) -> string {
    let out = "";
    let i = 0;
    while i < n {
        let r = locate(i) ?? new Cut(-1, -1);
        // Building a string is a safepoint the joined object is live at.
        out = out + show(r.lo) + ":" + show(r.hi - r.lo) + ",";
        i = i + 1;
    }
    return out;
}

fn main() {
    let text = spans(12) + " | " + kept(9) + " | " + written(12) + " | " + show(same(12)) + " | " +
               labels(5);
    let _ = write(stdout(), to_bytes(text));
}
