// @needs sockets
// Sockets on 127.0.0.1: a TCP echo between a server task and a client task,
// peer and local addresses that match, a half-close seen as end of stream,
// and a UDP datagram with its sender's port.
use tests::assert;
use io::close;
use io::write;
use io::IoError;
use io::sync::accept_wait;
use io::sync::read_to_end;
use io::sync::recv_from_wait;
use io::sync::write_all;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use io::net::tcp::peer_addr;
use io::net::tcp::shutdown;
use io::net::udp::bind;
use io::net::udp::local_port;
use io::net::udp::send_to;
use string::{from_bytes, to_bytes, StringError};
use task::{scope, Scope, TaskError};

fn serve_once(Stream listener) -> Result<string, IoError> {
    let conn = accept_wait(listener)?;
    let got = from_bytes(read_to_end(conn)?).map_err(fn (StringError e) => IoError::InvalidInput)?;
    write_all(conn, to_bytes("echo:" + got))?;
    close(conn)?;
    return got;
}

fn call(int port, string msg) -> Result<string, IoError> {
    let c = connect("127.0.0.1", port)?;
    write_all(c, to_bytes(msg))?;
    shutdown(c, 1)?;
    let reply = from_bytes(read_to_end(c)?).map_err(fn (StringError e) => IoError::InvalidInput)?;
    close(c)?;
    return reply;
}

fn text_of(Result<Result<string, IoError>, TaskError> r) -> string {
    return match r {
        Result::Ok(Result::Ok(s)) => s,
        Result::Ok(Result::Err(e)) => "io error " + e.show(),
        Result::Err(_) => "task failed",
    };
}

test("a TCP echo between two tasks") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    assert!(port > 0);
    let r = scope(fn (Scope s) use (listener, port) {
        let server = s.spawn(fn () use (listener) => serve_once(listener));
        let client = s.spawn(fn () use (port) => call(port, "ping"));
        text_of(server.join()) + "|" + text_of(client.join())
    });
    let both = match r {
        Result::Ok(t) => t,
        Result::Err(_) => "scope failed",
    };
    assert!(both == "ping|echo:ping", both);
    close(listener)?;
}

test("peer_addr of a connection is the other end's local_addr") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let c = connect("127.0.0.1", port)?;
    let conn = accept_wait(listener)?;
    let (chost, cport) = local_addr(c)?;
    let (phost, pport) = peer_addr(conn)?;
    assert!(chost == "127.0.0.1" && phost == chost);
    assert!(cport == pport);
    assert!(peer_addr(c)?[1] == port);
    close(c)?;
    close(conn)?;
    close(listener)?;
}

test("a UDP datagram arrives with its sender's port") {
    let a = bind("127.0.0.1", 0)?;
    let b = bind("127.0.0.1", 0)?;
    let pa = local_port(a)?;
    let pb = local_port(b)?;
    assert!(pa > 0 && pb > 0 && pa != pb);
    assert!(send_to(a, to_bytes("hey"), "127.0.0.1", pb)? == 3);
    let z: byte = 0;
    let buf = Vec::from([z, z, z, z, z, z, z, z]);
    let (n, host, port) = recv_from_wait(b, buf)?;
    assert!(n == 3);
    assert!(host == "127.0.0.1" && port == pa);
    assert!(buf[0] == 104 as byte && buf[2] == 121 as byte);
    close(a)?;
    close(b)?;
}
