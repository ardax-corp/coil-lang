// COI-404: module-sized Result<string, string> next to colliding short names.
use string::{to_bytes, from_bytes};
use coi404_ascii::{to_lower as ascii_lower};

fn utf8_ok(Vec<byte> b) -> Result<string, string> {
    return match from_bytes(b) {
        Result::Ok(s) => s,
        Result::Err(_) => raise "utf8",
    };
}

fn to_lower(string s) -> Result<string, string> {
    let b = to_bytes(s);
    let out: Vec<byte> = Vec::new();
    let i = 0;
    let a_up: byte = "A";
    let z_up: byte = "Z";
    while i < len(b) {
        let c = b[i];
        if c >= a_up {
            if c <= z_up {
                out.push(ascii_lower(c));
            }
            if c > z_up {
                out.push(c);
            }
        }
        if c < a_up {
            out.push(c);
        }
        i = i + 1;
    }
    return utf8_ok(out)?;
}

fn replace_mark(string s) -> Result<string, string> {
    let b = to_bytes(s);
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < len(b) {
        out.push(b[i]);
        i = i + 1;
    }
    out.push("x");
    return utf8_ok(out)?;
}
