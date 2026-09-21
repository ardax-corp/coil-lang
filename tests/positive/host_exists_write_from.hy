// COI-400: host Result match for exists / write_from (coil-stdlib io tests).
use io::{open, close, write_from, write, IoError};
use io::fs::{exists, remove_file};
use string::{to_bytes, from_bytes};

test("exists matches Ok true after write") {
    let path = "coil_lang_exists_roundtrip.txt";
    let s = match open(path, "w") {
        Result::Ok(v) => v,
        Result::Err(_) => panic "open w",
    };
    match write(s, to_bytes("hello")) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "write",
    };
    match close(s) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "close w",
    };
    let ex = match exists(path) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "exists",
    };
    assert(ex)?;
    match remove_file(path) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "remove",
    };
}

test("exists of cwd matches Ok") {
    match exists(".") {
        Result::Ok(v) => assert(v)?,
        Result::Err(_) => panic "exists cwd",
    };
}

test("write_from mid offset matches Ok") {
    let path = "coil_lang_write_from.txt";
    let s = match open(path, "w") {
        Result::Ok(v) => v,
        Result::Err(_) => panic "open w",
    };
    let buf = to_bytes("XXXhello");
    match write_from(s, buf, 3) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "write_from",
    };
    match close(s) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "close w",
    };
    match remove_file(path) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "remove",
    };
}

test("write_from at len is Ok zero") {
    let path = "coil_lang_write_from_len.txt";
    let s = match open(path, "w") {
        Result::Ok(v) => v,
        Result::Err(_) => panic "open w",
    };
    let buf = to_bytes("abcd");
    match write_from(s, buf, 4) {
        Result::Ok(n) => assert(n == 0)?,
        Result::Err(_) => panic "at len",
    };
    match close(s) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "close",
    };
    match remove_file(path) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "remove",
    };
}
