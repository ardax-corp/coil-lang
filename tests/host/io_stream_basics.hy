// File streams: `read` fills a buffer and reports `None` at end of file,
// `write` returns the bytes written, the `"w"`, `"a"` and `"rw"` modes
// truncate, append and overwrite in place, an unknown mode is `InvalidInput`
// and a missing file is `NotFound`. `IoError` shows as its variant name.
use tests::assert;
use io::open;
use io::close;
use io::read;
use io::write;
use io::IoError;
use io::fs::exists;
use io::fs::remove_file;
use io::file::read_text;
use string::{format, from_bytes, to_bytes};

fn err_name<T>(Result<T, IoError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => format("%v", e),
    };
}

fn zeros(int n) -> Vec<byte> {
    let z: byte = 0;
    let v: Vec<byte> = Vec::new();
    for _ in 0..n {
        v.push(z);
    }
    return v;
}

test("write returns the byte count; read until None") {
    let path = "coil_host_stream_basic.txt";
    let w = open(path, "w")?;
    assert!(write(w, to_bytes("hello world"))? == 11);
    close(w)?;
    let r = open(path, "r")?;
    let buf = zeros(4);
    let got = "";
    let reads = 0;
    let done = false;
    while !done {
        let got_n = read(r, buf)?;
        match got_n {
            Option::Some(n) => {
                let part: Vec<byte> = Vec::new();
                for i in 0..n {
                    part.push(buf[i]);
                }
                got = got + from_bytes(part)?;
                reads += 1;
            },
            Option::None => {
                done = true;
            },
        }
    }
    close(r)?;
    assert!(got == "hello world");
    assert!(reads == 3);
    remove_file(path)?;
}

test("append mode adds to the end") {
    let path = "coil_host_stream_append.txt";
    let a = open(path, "w")?;
    write(a, to_bytes("one"))?;
    close(a)?;
    let b = open(path, "a")?;
    write(b, to_bytes("+two"))?;
    close(b)?;
    assert!(read_text(path)? == "one+two");
    remove_file(path)?;
}

test("mode rw reads and writes without truncating") {
    let path = "coil_host_stream_rw.txt";
    let a = open(path, "w")?;
    write(a, to_bytes("abcdef"))?;
    close(a)?;
    let b = open(path, "rw")?;
    write(b, to_bytes("XY"))?;
    close(b)?;
    assert!(read_text(path)? == "XYcdef");
    remove_file(path)?;
}

test("an unknown mode is InvalidInput") {
    assert!(err_name(open("coil_host_stream_bad_mode.txt", "q")) == "InvalidInput");
    assert!(!exists("coil_host_stream_bad_mode.txt")?);
}

test("opening a missing file for reading is NotFound") {
    assert!(err_name(open("coil_host_no_such_file.txt", "r")) == "NotFound");
}

test("IoError shows as its variant name") {
    let e = IoError::TimedOut;
    assert!(format("%v", e) == "TimedOut");
    assert!(e.show() == "TimedOut");
}

test("an empty file reads None at once") {
    let path = "coil_host_stream_empty.txt";
    close(open(path, "w")?)?;
    let r = open(path, "r")?;
    let got_n = read(r, zeros(8))?;
    let first = match got_n {
        Option::Some(n) => n,
        Option::None => -1,
    };
    close(r)?;
    assert!(first == -1);
    remove_file(path)?;
}
