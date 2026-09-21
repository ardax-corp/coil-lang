// COI-400: heap-heap niche Result match / newly built buffers
// (coil-stdlib path join + text replace/to_lower).
use io::{IoError};
use string::{from_bytes, to_bytes};

class Path {
    pub raw: string,
}

impl Path {
    pub static fn from(string s) -> Path {
        return new Path(s);
    }

    pub fn as_str() -> string {
        return self.raw;
    }

    pub fn join(Path other) -> Result<Path, IoError> {
        let a = to_bytes(self.raw);
        let b = to_bytes(other.raw);
        let slash: Vec<byte> = Vec::new();
        slash.push("/");
        let mid = Vec::new();
        let i = 0;
        while i < len(a) {
            mid.push(a[i]);
            i = i + 1;
        }
        let j = 0;
        while j < len(slash) {
            mid.push(slash[j]);
            j = j + 1;
        }
        let k = 0;
        while k < len(b) {
            mid.push(b[k]);
            k = k + 1;
        }
        return match from_bytes(mid) {
            Result::Ok(s) => new Path(s),
            Result::Err(e) => raise e,
        };
    }
}

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
    while i < len(b) {
        out.push(b[i]);
        i = i + 1;
    }
    out.push("x");
    return utf8_ok(out)?;
}

fn replace_two(string s) -> Result<string, string> {
    let b = to_bytes(s);
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < len(b) {
        if i + 2 < len(b) && b[i] == "t" && b[i + 1] == "w" && b[i + 2] == "o" {
            out.push("2");
            i = i + 3;
        } else {
            out.push(b[i]);
            i = i + 1;
        }
    }
    return utf8_ok(out)?;
}

test("result path join matches Ok") {
    let a = Path::from("a");
    let b = Path::from("b");
    let j = match a.join(b) {
        Result::Ok(p) => p,
        Result::Err(_) => panic "join",
    };
    assert(j.as_str() == "a/b")?;
}

test("result string string from newly built buffer") {
    let low = match to_lower("AbC") {
        Result::Ok(s) => s,
        Result::Err(_) => panic "lower",
    };
    assert(low == "AbCx")?;
}

test("result string string question on new buffer") {
    assert(replace_two("one two two")? == "one 2 2")?;
}

test("host from_bytes of new buffer still Ok") {
    let b = to_bytes("hi");
    match from_bytes(b) {
        Result::Ok(s) => {
            assert(s == "hi")?;
        },
        Result::Err(_) => {
            panic "from_bytes";
        },
    };
}
