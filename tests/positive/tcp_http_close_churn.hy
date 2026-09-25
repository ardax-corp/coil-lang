// COI-410: sequential TCP HTTP/1.1 GETs with Connection: close (no pool).
// Cooperative server/client so each GET parks on await_readable then discards.
use io::{close, read, await_readable, Stream};
use io::sync::{write_all};
use io::net::tcp::{accept, connect, listen, local_addr};
use string::{to_bytes};

fn read_http(Stream s) -> Result<int, IoError> {
    let z: byte = 0;
    let chunk: Vec<byte> = Vec::new();
    let i = 0;
    while i < 64 {
        chunk.push(z);
        i = i + 1;
    }
    let acc: Vec<byte> = Vec::new();
    let guard = 0;
    let done = 0;
    while done == 0 {
        if guard >= 256 {
            done = 1;
        }
        if done == 0 {
            let parked = 0;
            let nopt = match read(s, chunk) {
                Result::Ok(o) => o,
                Result::Err(IoError::WouldBlock) => {
                    match await_readable(s) {
                        Result::Ok(_) => {
                            parked = 1;
                            guard = guard - 1;
                            Option::None
                        },
                        Result::Err(_) => panic "await_readable treated Ok as Err",
                    }
                },
                Result::Err(_) => panic "read",
            };
            if parked == 0 {
                match nopt {
                    Option::None => {
                        if len(acc) > 0 {
                            done = 1;
                        }
                    },
                    Option::Some(n) => {
                        if n == 0 {
                            match await_readable(s) {
                                Result::Ok(_) => {
                                    guard = guard - 1;
                                    0
                                },
                                Result::Err(_) => panic "await0",
                            };
                        }
                        if n != 0 {
                            let j = 0;
                            while j < n {
                                acc.push(chunk[j]);
                                j = j + 1;
                            }
                        }
                    },
                };
            }
        }
        guard = guard + 1;
    }
    return len(acc);
}

async fn serve(Stream listener) -> int {
    let n = 0;
    while n < 200 {
        match accept(listener) {
            Result::Ok(s) => {
                match write_all(s, to_bytes("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")) {
                    Result::Ok(_) => 0,
                    Result::Err(_) => panic "server write",
                };
                match close(s) {
                    Result::Ok(_) => 0,
                    Result::Err(_) => 0,
                };
                n = n + 1;
            },
            Result::Err(_) => {
                match await_readable(listener) {
                    Result::Ok(_) => 0,
                    Result::Err(_) => panic "listen wait",
                };
            },
        };
    }
    return n;
}

async fn client(int port) -> int {
    let i = 0;
    let total = 0;
    while i < 200 {
        let s = match connect("127.0.0.1", port) {
            Result::Ok(v) => v,
            Result::Err(_) => panic "connect",
        };
        match write_all(s, to_bytes("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")) {
            Result::Ok(_) => 0,
            Result::Err(_) => panic "write",
        };
        let n = match read_http(s) {
            Result::Ok(v) => v,
            Result::Err(_) => panic "read_http",
        };
        if n < 10 {
            panic "short";
        }
        match close(s) {
            Result::Ok(_) => 0,
            Result::Err(_) => 0,
        };
        total = total + 2;
        i = i + 1;
    }
    return total;
}

test("200 sequential Connection-close GETs park then discard") {
    let listener = match listen("127.0.0.1", 0) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "listen",
    };
    let addr = match local_addr(listener) {
        Result::Ok(a) => a,
        Result::Err(_) => panic "addr",
    };
    let srv = serve(listener);
    let cli = client(addr[1]);
    resume srv;
    let total = resume cli;
    while !done(srv) || !done(cli) {
        if !done(srv) {
            resume srv;
        }
        if !done(cli) {
            total = resume cli;
        }
        wait_ready();
    }
    assert(total == 400, "body bytes")?;
    match close(listener) {
        Result::Ok(_) => 0,
        Result::Err(_) => 0,
    };
}
