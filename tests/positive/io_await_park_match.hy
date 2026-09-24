// COI-408: park on WouldBlock, resume, match Result::Ok (not boxed-as-Err).
use io::{await_readable, await_writable, close, read, wait_ready, write};
use io::net::tcp::{accept, connect, listen, local_addr};
use string::{to_bytes};

fn connected_pair() -> Result<(Stream, Stream, Stream), IoError> {
    let listener = listen("127.0.0.1", 0)?;
    let addr = local_addr(listener)?;
    let client = connect("127.0.0.1", addr[1])?;
    match await_readable(listener) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "listen wait",
    };
    let server = accept(listener)?;
    return Result::Ok((client, server, listener));
}

async fn http_read_after_wait(Stream c) -> int {
    let z: byte = 0;
    let buf = Vec::from([z, z, z, z, z, z, z, z]);
    match read(c, buf) {
        Result::Ok(_) => panic "first read should WouldBlock",
        Result::Err(_) => {
            match await_readable(c) {
                Result::Ok(_) => {},
                Result::Err(_) => panic "await_readable treated Ok as Err",
            };
        },
    };
    return match read(c, buf) {
        Result::Ok(got) => match got {
            Option::Some(n) => n,
            Option::None => panic "eof before body",
        },
        Result::Err(_) => panic "second read after wait",
    };
}

async fn http_write_response(Stream s) -> int {
    return match write(s, to_bytes("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "server write",
    };
}

test("await_readable match Ok after WouldBlock park") {
    let triple = match connected_pair() {
        Result::Ok(v) => v,
        Result::Err(_) => panic "pair",
    };
    let c = triple[0];
    let s = triple[1];
    let listener = triple[2];
    let reader = http_read_after_wait(c);
    let writer = http_write_response(s);
    resume reader;
    resume writer;
    wait_ready();
    let n = resume reader;
    assert(n > 0)?;
    match close(c) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close client",
    };
    match close(s) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close server",
    };
    match close(listener) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close listener",
    };
}

test("await_writable match Ok on connected socket") {
    let triple = match connected_pair() {
        Result::Ok(v) => v,
        Result::Err(_) => panic "pair",
    };
    let c = triple[0];
    let s = triple[1];
    let listener = triple[2];
    match await_writable(c) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "await_writable treated Ok as Err",
    };
    match close(c) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close client",
    };
    match close(s) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close server",
    };
    match close(listener) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close listener",
    };
}
