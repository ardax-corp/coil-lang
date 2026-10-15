// `let server = match accept_wait(listener) { … }` inside a `while` loop
// serves one connection per trip: two clients each read the reply.
use tests::assert;
use io::close;
use io::read;
use io::write;
use io::IoError;
use io::sync::accept_wait;
use io::sync::read_to_end;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use string::{from_bytes, to_bytes};
use task::{scope, Scope, TaskError};

fn serve_two(Stream listener) -> int {
    let msg = to_bytes("ok");
    let count = 0;
    while count < 2 {
        let server = match accept_wait(listener) {
            Result::Ok(s) => s,
            Result::Err(_) => panic "accept",
        };
        match write(server, msg) {
            Result::Ok(_) => 0,
            Result::Err(_) => panic "write",
        }
        match close(server) {
            Result::Ok(_) => 0,
            Result::Err(_) => 0,
        }
        count = count + 1;
    }
    return count;
}

fn fetch(int port) -> Result<string, IoError> {
    let c = connect("127.0.0.1", port)?;
    let got = from_bytes(read_to_end(c)?)?;
    close(c)?;
    return got;
}

fn two_fetches(int port) -> string {
    let a = match fetch(port) {
        Result::Ok(s) => s,
        Result::Err(e) => "io error " + e.show(),
    };
    let b = match fetch(port) {
        Result::Ok(s) => s,
        Result::Err(e) => "io error " + e.show(),
    };
    return a + "|" + b;
}

test("a let-bound accept match in a loop handles two connections") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let r = scope(
        fn (Scope s) use (listener, port) {
            let server = s.spawn(fn () use (listener) => serve_two(listener));
            let client = s.spawn(fn () use (port) => two_fetches(port));
            let served = match server.join() {
                Result::Ok(n) => n,
                Result::Err(_) => -1,
            };
            let got = match client.join() {
                Result::Ok(t) => t,
                Result::Err(_) => "client failed",
            };
            (served, got)
        },
    );
    let (served, got) = match r {
        Result::Ok(p) => p,
        Result::Err(_) => (-1, "scope failed"),
    };
    assert!(served == 2);
    assert!(got == "ok|ok", got);
    close(listener)?;
}
