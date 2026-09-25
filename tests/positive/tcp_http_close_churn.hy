// COI-410: sequential TCP HTTP/1.1 GETs with Connection: close (no pool).
// One pair per request: park the client on await_readable, then discard.
use io::{await_readable, close, read, wait_ready, write};
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
    let buf = Vec::from([z, z, z, z, z, z, z, z, z, z, z, z, z, z, z, z]);
    let got = 0;
    let spins = 0;
    while got == 0 {
        if spins >= 64 {
            panic "read timeout";
        }
        match read(c, buf) {
            Result::Ok(nopt) => match nopt {
                Option::Some(n) => {
                    if n > 0 {
                        got = n;
                    }
                },
                Option::None => panic "eof before body",
            },
            Result::Err(IoError::WouldBlock) => {
                match await_readable(c) {
                    Result::Ok(_) => {},
                    Result::Err(_) => panic "await_readable treated Ok as Err",
                };
            },
            Result::Err(_) => panic "read",
        };
        spins = spins + 1;
    }
    return got;
}

test("200 sequential Connection-close GETs park then discard") {
    let i = 0;
    let total = 0;
    while i < 200 {
        let triple = match connected_pair() {
            Result::Ok(v) => v,
            Result::Err(_) => panic "pair",
        };
        let c = triple[0];
        let s = triple[1];
        let listener = triple[2];
        let reader = http_read_after_wait(c);
        let n = resume reader;
        match write(s, to_bytes("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")) {
            Result::Ok(_) => {},
            Result::Err(_) => panic "server write",
        };
        while !done(reader) {
            wait_ready();
            n = resume reader;
        }
        if n < 2 {
            panic "short";
        }
        total = total + 2;
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
        i = i + 1;
    }
    assert(total == 400)?;
}
