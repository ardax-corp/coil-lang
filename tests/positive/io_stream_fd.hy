// Stream.fd() is the explicit marshal after silent Stream→Int FFI was removed.
// `?` inside Result<int, IoError> must not treat the HostInvoke thunk as a
// two-slot CALL (that made TLS enable see InvalidInput on a live socket).
use io::stdout;
use io::Stream;
use io::IoError;
use io::net::tcp::listen;
use io::net::tcp::connect;
use io::net::tcp::local_addr;

fn fd_via_try(Stream s) -> Result<int, IoError> {
    return s.fd()?;
}

test("stdout has a fd") {
    assert(stdout().fd()? >= 0)?;
}

test("tcp listener fd is distinct from a connected client") {
    let srv = listen("127.0.0.1", 0)?;
    let lfd = srv.fd()?;
    assert(lfd >= 0)?;
    let addr = local_addr(srv)?;
    let (_, port) = addr;
    let cli = connect("127.0.0.1", port)?;
    let cfd = cli.fd()?;
    assert(cfd >= 0)?;
    assert(cfd != lfd)?;
    let via = fd_via_try(cli)?;
    assert(via == cfd)?;
}
