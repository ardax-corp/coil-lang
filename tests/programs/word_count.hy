// Splitting words over bytes, longest word and counts in one pass, ASCII upper-casing and reversal.
// @output words=7 longest=quick the=2 upper=THE QUICK reversed=kciuq\n
use io::sync::println;
use string::{format, to_bytes, from_bytes};

fn split_words(string s) -> Vec<string> {
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

fn upper(string s) -> string {
    let shift: byte = 32;
    let out: Vec<byte> = Vec::new();
    for b in to_bytes(s) {
        if b >= "a" && b <= "z" {
            out.push(b - shift);
        } else {
            out.push(b);
        }
    }
    return from_bytes(out).unwrap_or(s);
}

fn reverse(string s) -> string {
    let bs = to_bytes(s);
    let out: Vec<byte> = Vec::new();
    let i = bs.len();
    while i > 0 {
        i -= 1;
        out.push(bs[i]);
    }
    return from_bytes(out).unwrap_or("");
}

fn main() {
    let text = "the quick brown fox jumps over the";
    let words = split_words(text);
    let longest = "";
    let the = 0;
    for w in words {
        if w.len() > longest.len() {
            longest = w;
        }
        if w == "the" {
            the += 1;
        }
    }
    let _ = println(
        format(
            "words=%i longest=%s the=%i upper=%s reversed=%s",
            words.len(),
            longest,
            the,
            upper("the quick"),
            reverse("quick"),
        ),
    );
}
