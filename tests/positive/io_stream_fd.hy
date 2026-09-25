// Stream.fd() is the explicit marshal after silent Stream→Int FFI was removed.
// `?` inside Result<int, IoError> must not treat the HostInvoke thunk as a
// two-slot CALL (that made TLS enable see InvalidInput on a live socket).
use io::{stdout, Stream, IoError};
use io::net::tcp::{listen, connect, local_addr};

fn fd_via_try(Stream s) -> Result<int, IoError> {
    return s.fd()?;
}

test("stdout has a fd") {
    match stdout().fd() {
        Result::Ok(n) => {
            assert(n >= 0)?;
        },
        Result::Err(_) => {
            panic "stdout fd";
        },
    };
}

test("tcp listener fd is distinct from a connected client") {
    let srv = match listen("127.0.0.1", 0) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "listen",
    };
    let lfd = match srv.fd() {
        Result::Ok(n) => n,
        Result::Err(_) => panic "listener fd",
    };
    assert(lfd >= 0)?;
    let addr = match local_addr(srv) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "port",
    };
    let (_, port) = addr;
    let cli = match connect("127.0.0.1", port) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "connect",
    };
    let cfd = match cli.fd() {
        Result::Ok(n) => n,
        Result::Err(_) => panic "client fd",
    };
    assert(cfd >= 0)?;
    assert(cfd != lfd)?;
    let via = match fd_via_try(cli) {
        Result::Ok(n) => n,
        Result::Err(_) => panic "fd try",
    };
    assert(via == cfd)?;
}
