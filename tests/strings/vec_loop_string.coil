// Map loops whose stream is a `string` run 8 lanes at a time
// (`mir/vec_loop.rs`, #2150): a push, a push per arm, a store and a
// store per arm. Each is checked against the same loop kept scalar
// (`black_box` in its body), at every length up to 40 bytes, ASCII and
// not, and from several starts.
use tests::assert;
use string::to_bytes;
use thread::{join, spawn, ThreadError};

fn shift(string s, byte lo, byte hi, int delta) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < s.len() {
        let c = s[i];
        if c >= lo && c <= hi {
            out.push(((c as int) + delta) as byte);
        } else {
            out.push(c);
        }
        i = i + 1;
    }
    return out;
}

fn shift_scalar(string s, byte lo, byte hi, int delta) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < s.len() {
        let c = black_box(s[i]);
        if c >= lo && c <= hi {
            out.push(((c as int) + delta) as byte);
        } else {
            out.push(c);
        }
        i = i + 1;
    }
    return out;
}

fn codes(string s, int from, int to) -> Vec<int> {
    let out: Vec<int> = [-1];
    let i = from;
    while i < to {
        out.push((s[i] as int) * 3 + 1);
        i = i + 1;
    }
    return out;
}

fn codes_scalar(string s, int from, int to) -> Vec<int> {
    let out: Vec<int> = [-1];
    let i = from;
    while i < to {
        out.push((black_box(s[i]) as int) * 3 + 1);
        i = i + 1;
    }
    return out;
}

fn pair_sums(string s) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    let i = 0;
    let n = s.len() - 1;
    while i < n {
        out.push((s[i] as int) + (s[i + 1] as int));
        i = i + 1;
    }
    return out;
}

fn pair_sums_scalar(string s) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    let i = 0;
    let n = s.len() - 1;
    while i < n {
        out.push((black_box(s[i]) as int) + (s[i + 1] as int));
        i = i + 1;
    }
    return out;
}

fn store_codes(Vec<int> out, string s) {
    let i = 0;
    while i < s.len() {
        out[i] = (s[i] as int) * 2 + 1;
        i = i + 1;
    }
}

fn store_codes_scalar(Vec<int> out, string s) {
    let i = 0;
    while i < s.len() {
        out[i] = (black_box(s[i]) as int) * 2 + 1;
        i = i + 1;
    }
}

fn lower_into(Vec<byte> out, string s) {
    let i = 0;
    while i < s.len() {
        let c = s[i];
        if c >= "A" && c <= "Z" {
            out[i] = ((c as int) + 32) as byte;
        } else {
            out[i] = c;
        }
        i = i + 1;
    }
}

fn lower_into_scalar(Vec<byte> out, string s) {
    let i = 0;
    while i < s.len() {
        let c = black_box(s[i]);
        if c >= "A" && c <= "Z" {
            out[i] = ((c as int) + 32) as byte;
        } else {
            out[i] = c;
        }
        i = i + 1;
    }
}

// `n` bytes of `pieces`, cycling; a piece cut short ends the string
// early, so take what fits.
fn text(int n, bool ascii) -> string {
    let pieces = if ascii {
        ["Hello", ", ", "WORLD", "! ", "abcXYZ", "019 "]
    } else {
        ["Héllo", ", ", "wÖrld", "! ", "été", "ZZ"]
    };
    let s = "";
    let k = 0;
    while s.len() < n {
        let p = pieces[k % pieces.len()];
        if s.len() + p.len() > n {
            if ascii {
                s = s + "Q";
            } else {
                return s;
            }
        } else {
            s = s + p;
        }
        k = k + 1;
    }
    return s;
}

test("string map-push and store maps match the scalar loop") {
    for ascii in [true, false] {
        for n in 0..41 {
            let s = text(n, ascii);
            if ascii {
                assert!(s.len() == n);
            }
            let a = shift(s, "A" as byte, "Z" as byte, 32);
            assert!(a.len() == s.len());
            assert!(a == shift_scalar(s, "A" as byte, "Z" as byte, 32));
            let b = shift(s, "a" as byte, "z" as byte, -32);
            assert!(b == shift_scalar(s, "a" as byte, "z" as byte, -32));
            assert!(codes(s, 0, s.len()) == codes_scalar(s, 0, s.len()));
            if s.len() > 3 {
                assert!(codes(s, 3, s.len()) == codes_scalar(s, 3, s.len()));
                assert!(codes(s, 1, s.len() - 2) == codes_scalar(s, 1, s.len() - 2));
            }
            if s.len() > 0 {
                let p = pair_sums(s);
                assert!(p.len() == s.len() - 1);
                assert!(p == pair_sums_scalar(s));
            }
            let x: Vec<int> = Vec::new();
            let y: Vec<int> = Vec::new();
            let u = to_bytes(s);
            let v = to_bytes(s);
            let k = 0;
            while k < s.len() {
                x.push(0);
                y.push(0);
                u[k] = 0 as byte;
                v[k] = 0 as byte;
                k = k + 1;
            }
            store_codes(x, s);
            store_codes_scalar(y, s);
            assert!(x == y);
            lower_into(u, s);
            lower_into_scalar(v, s);
            assert!(u == v);
        }
    }
}

test("non-ASCII bytes keep their values") {
    let s = "aé€Z";
    let a = shift(s, "A" as byte, "Z" as byte, 32);
    assert!(a.len() == 7);
    assert!(a[0] == "a" && a[1] == 195 as byte && a[2] == 169 as byte && a[6] == "z");
    let c = codes(s, 0, s.len());
    assert!(c.len() == 8 && c[0] == -1 && c[2] == 195 * 3 + 1 && c[4] == 226 * 3 + 1);
}

test("an empty string maps to nothing") {
    assert!(shift("", "A" as byte, "Z" as byte, 32).len() == 0);
    assert!(codes("", 0, 0).len() == 1);
}

fn codes_past_end() -> int {
    return codes("abcdefghijklmnopqrstuvwxyz", 2, 30).len();
}

fn codes_before_start() -> int {
    return codes("abcdefghijklmnopqrstuvwxyz", -1, 20).len();
}

fn panics(() -> int f) -> bool {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Err(ThreadError::JoinFailed) => true,
        default => false,
    };
}

test("a read out of range still panics") {
    assert!(panics(codes_past_end));
    assert!(panics(codes_before_start));
}
