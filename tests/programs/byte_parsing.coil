// Hand-written integer / hex parsing and splitting over `to_bytes` / `from_bytes`, with `raise` for bad input.
// @output 1234 -56 err:empty [3, 14, 15] 0x1f=31 key=value\n
use io::sync::println;
use string::{format, to_bytes, from_bytes};

fn parse_int(string s) -> Result<int, string> {
    let zero: byte = "0";
    let bs = to_bytes(s);
    if bs.len() == 0 {
        raise "empty";
    }
    let neg = bs[0] == "-";
    let i = if neg {
        1
    } else {
        0
    };
    let n = 0;
    while i < bs.len() {
        let b = bs[i];
        if b < "0" || b > "9" {
            raise format("bad digit at %i", i);
        }
        n = n * 10 + (b - zero) as int;
        i += 1;
    }
    return if neg {
        -n
    } else {
        n
    };
}

fn split(string s, byte sep) -> Vec<string> {
    let out: Vec<string> = Vec::new();
    let cur: Vec<byte> = Vec::new();
    for b in to_bytes(s) {
        if b == sep {
            out.push(from_bytes(cur).unwrap_or(""));
            cur = Vec::new();
        } else {
            cur.push(b);
        }
    }
    out.push(from_bytes(cur).unwrap_or(""));
    return out;
}

fn parse_hex(string s) -> int {
    let zero: byte = "0";
    let a: byte = "a";
    let n = 0;
    for b in to_bytes(s) {
        let d = if b >= "0" && b <= "9" {
            (b - zero) as int
        } else if b >= "a" && b <= "f" {
            (b - a) as int + 10
        } else {
            0
        };
        n = n * 16 + d;
    }
    return n;
}

fn main() {
    let a = parse_int("1234").unwrap_or(0);
    let b = parse_int("-56").unwrap_or(0);
    let e = match parse_int("") {
        Result::Ok(v) => format("%i", v),
        Result::Err(m) => "err:" + m,
    };
    let nums: Vec<int> = Vec::new();
    for part in split("3,14,15", ",") {
        nums.push(parse_int(part).unwrap_or(-1));
    }
    let kv = split("key=value", "=");
    let _ = println(
        format("%i %i %s %v 0x1f=%i %s=%s", a, b, e, nums, parse_hex("1f"), kv[0], kv[1]),
    );
}
