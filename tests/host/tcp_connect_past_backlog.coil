// @needs sockets
// 200 client tasks connect against a 128-entry listen backlog: the ones whose
// SYN is dropped wait in their own task while the server task keeps
// accepting (coil-lang#832). Counts connections only.
use task::{scope, Scope, TaskError};
use io::close;
use io::Stream;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use io::sync::accept_wait;

fn connect_close(int port) -> int {
    let c = match connect("127.0.0.1", port) {
        Result::Ok(c) => c,
        Result::Err(_) => panic "connect",
    };
    let _ = close(c);
    return 0;
}

fn accepted(Result<int, TaskError> r) -> int {
    return match r {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

test("connects past the listen backlog suspend only their own task") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let r = scope(fn (Scope s) use (listener, port) {
        let server = s.spawn(fn () use (listener) {
            let n = 0;
            while n < 200 {
                let c = match accept_wait(listener) {
                    Result::Ok(c) => c,
                    Result::Err(_) => panic "accept",
                };
                let _ = close(c);
                n = n + 1;
            }
            n
        });
        let i = 0;
        while i < 200 {
            s.spawn(fn () use (port) => connect_close(port));
            i = i + 1;
        }
        accepted(server.join())
    });
    if accepted(r) != 200 {
        raise "not all accepted";
    }
}
