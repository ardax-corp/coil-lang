// COI-408: park on WouldBlock, resume, match Result::Ok (not boxed-as-Err).
use io::{wait_readable, wait_writable, close, read};
use task::{scope, Scope};
use io::write;
use io::net::tcp::accept;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use string::to_bytes;

fn connected_pair() -> Result<(Stream, Stream, Stream), IoError> {
    let listener = listen("127.0.0.1", 0)?;
    let addr = local_addr(listener)?;
    let client = connect("127.0.0.1", addr[1])?;
    wait_readable(listener)?;
    let server = accept(listener)?;
    return Result::Ok((client, server, listener));
}

gen fn http_read_after_wait(Stream c) -> int {
    let z: byte = 0;
    let buf = Vec::from([z, z, z, z, z, z, z, z]);
    match read(c, buf) {
        Result::Ok(_) => panic "first read should WouldBlock",
        Result::Err(_) => {
            match wait_readable(c) {
                Result::Ok(_) => {},
                Result::Err(_) => panic "wait_readable treated Ok as Err",
            }
        },
    }
    return match read(c, buf) {
        Result::Ok(got) => match got {
            Option::Some(n) => n,
            Option::None => panic "eof before body",
        },
        Result::Err(_) => panic "second read after wait",
    };
}

gen fn http_write_response(Stream s) -> int {
    return match write(s, to_bytes("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")) {
        Result::Ok(_) => 0,
        Result::Err(_) => panic "server write",
    };
}

test("wait_readable match Ok after WouldBlock park") {
    let triple = connected_pair()?;
    let c = triple[0];
    let s = triple[1];
    let listener = triple[2];
    // The reader parks inside its generator; that suspends its task, so the
    // writer task runs and the reader's wait completes.
    let r = scope(
        fn (Scope sc) use (c, s) {
            let reader = sc
                .spawn(
                    fn () use (c) {
                        let g = http_read_after_wait(c);
                        resume g
                    },
                );
            sc
                .spawn(
                    fn () use (s) {
                        let g = http_write_response(s);
                        resume g
                    },
                );
            match reader.join() {
                Result::Ok(n) => n,
                Result::Err(_) => -1,
            }
        },
    );
    let n = match r {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
    assert(n > 0)?;
    close(c)?;
    close(s)?;
    close(listener)?;
}

test("wait_writable match Ok on connected socket") {
    let triple = connected_pair()?;
    let c = triple[0];
    let s = triple[1];
    let listener = triple[2];
    match wait_writable(c) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "wait_writable treated Ok as Err",
    }
    close(c)?;
    close(s)?;
    close(listener)?;
}
