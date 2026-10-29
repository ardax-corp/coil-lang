// Roman numerals both ways, a byte-level `starts_with`, splitting on spaces and joining with a separator.
// @output XIV MCMXCIV 1994 true [the, cat] a-b-c\n
use io::sync::println;
use string::{format, to_bytes, from_bytes};

fn to_roman(int n) -> string {
    let vals = [1000, 900, 500, 400, 100, 90, 50, 40, 10, 9, 5, 4, 1];
    let syms = ["M", "CM", "D", "CD", "C", "XC", "L", "XL", "X", "IX", "V", "IV", "I"];
    let out = "";
    let i = 0;
    while i < vals.len() {
        while n >= vals[i] {
            out += syms[i];
            n -= vals[i];
        }
        i += 1;
    }
    return out;
}

fn roman_value(byte c) -> int {
    return match c as int {
        73 => 1,
        86 => 5,
        88 => 10,
        76 => 50,
        67 => 100,
        68 => 500,
        77 => 1000,
        default => 0,
    };
}

fn from_roman(string s) -> int {
    let bs = to_bytes(s);
    let total = 0;
    let i = 0;
    while i < bs.len() {
        let v = roman_value(bs[i]);
        if i + 1 < bs.len() && v < roman_value(bs[i + 1]) {
            total -= v;
        } else {
            total += v;
        }
        i += 1;
    }
    return total;
}

fn join(Vec<string> parts, string sep) -> string {
    let out = "";
    let i = 0;
    for p in parts {
        if i > 0 {
            out += sep;
        }
        out += p;
        i += 1;
    }
    return out;
}

fn starts_with(string s, string prefix) -> bool {
    let a = to_bytes(s);
    let b = to_bytes(prefix);
    if b.len() > a.len() {
        return false;
    }
    let i = 0;
    while i < b.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    return true;
}

fn words(string s) -> Vec<string> {
    let out: Vec<string> = Vec::new();
    let cur: Vec<byte> = Vec::new();
    for b in to_bytes(s) {
        if b == " " {
            if cur.len() > 0 {
                out.push(from_bytes(cur).unwrap_or(""));
                cur = Vec::new();
            }
        } else {
            cur.push(b);
        }
    }
    if cur.len() > 0 {
        out.push(from_bytes(cur).unwrap_or(""));
    }
    return out;
}

fn main() {
    let _ = println(
        format(
            "%s %s %i %z %v %s",
            to_roman(14),
            to_roman(1994),
            from_roman("MCMXCIV"),
            starts_with("hello", "hel"),
            words("the cat"),
            join(Vec::from(["a", "b", "c"]), "-"),
        ),
    );
}
