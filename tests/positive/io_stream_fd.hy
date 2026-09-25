// Stream.fd() is the explicit marshal after silent Stream→Int FFI was removed.
use io::{stdout};
use io::net::tcp::{listen, connect, local_addr};

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
}
