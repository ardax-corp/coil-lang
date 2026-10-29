// Run-length encoding over bytes into a `Vec<(string, int)>`, rendered and decoded back with a tuple-destructuring `for`.
// @output 3a2b1c aaabbc true [(a, 3), (b, 2), (c, 1)]\n
use io::sync::println;
use string::{format, to_bytes, from_bytes};

fn encode(string s) -> Vec<(string, int)> {
    let out: Vec<(string, int)> = Vec::new();
    let bs = to_bytes(s);
    if bs.len() == 0 {
        return out;
    }
    let cur = bs[0];
    let n = 0;
    for b in bs {
        if b == cur {
            n += 1;
        } else {
            out.push((from_bytes(Vec::from([cur])).unwrap_or("?"), n));
            cur = b;
            n = 1;
        }
    }
    out.push((from_bytes(Vec::from([cur])).unwrap_or("?"), n));
    return out;
}

fn render(Vec<(string, int)> runs) -> string {
    let s = "";
    for (ch, n) in runs {
        s += format("%i%s", n, ch);
    }
    return s;
}

fn decode(Vec<(string, int)> runs) -> string {
    let s = "";
    for (ch, n) in runs {
        for _ in 0..n {
            s += ch;
        }
    }
    return s;
}

fn main() {
    let runs = encode("aaabbc");
    let _ = println(
        format("%s %s %z %v", render(runs), decode(runs), decode(runs) == "aaabbc", runs),
    );
}
