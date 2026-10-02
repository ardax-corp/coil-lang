// COI-400: host Result match for exists / write_from (coil-stdlib io tests).
use io::open;
use io::close;
use io::write_from;
use io::write;
use io::IoError;
use io::fs::exists;
use io::fs::remove_file;
use io::file::read_text;
use string::{to_bytes, from_bytes};

// The `exists` / `write_from` results are matched on purpose (the host
// Result match is what COI-400 fixed); the other calls just use `?`.
test("exists matches Ok true after write") {
    let path = "coil_lang_exists_roundtrip.txt";
    let s = open(path, "w")?;
    write(s, to_bytes("hello"))?;
    close(s)?;
    let ex = match exists(path) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "exists",
    };
    assert(ex)?;
    remove_file(path)?;
}

test("exists of cwd matches Ok") {
    match exists(".") {
        Result::Ok(v) => assert(v)?,
        Result::Err(_) => panic "exists cwd",
    }
}

test("write_from mid offset matches Ok") {
    let path = "coil_lang_write_from.txt";
    let s = open(path, "w")?;
    let buf = to_bytes("XXXhello");
    match write_from(s, buf, 3) {
        Result::Ok(n) => assert(n == 5)?,
        Result::Err(_) => panic "write_from",
    }
    close(s)?;
    assert(read_text(path)? == "hello")?;
    remove_file(path)?;
}

test("write_from at len is Ok zero") {
    let path = "coil_lang_write_from_len.txt";
    let s = open(path, "w")?;
    let buf = to_bytes("abcd");
    match write_from(s, buf, 4) {
        Result::Ok(n) => assert(n == 0)?,
        Result::Err(_) => panic "at len",
    }
    close(s)?;
    remove_file(path)?;
}
