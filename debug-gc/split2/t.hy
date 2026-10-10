// T1 tasks and IO: an IO wait suspends the task (never a generator it is
// resuming), so other tasks run while it waits.
use task::{scope, Scope, TaskError};
use thread::{spawn, join};
use clock::sleep_ms;
use io::close;
use io::Stream;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use io::net::tcp::shutdown;
use io::sync::accept_wait;
use io::sync::read_to_end;
use io::sync::write_all;
use string::to_bytes;

fn ok_or(Result<int, TaskError> r, int fallback) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => fallback,
    };
}

fn send_later(int port, int ms, string text) -> int {
    sleep_ms(ms);
    let c = match connect("127.0.0.1", port) {
        Result::Ok(c) => c,
        Result::Err(_) => panic "connect",
    };
    match write_all(c, to_bytes(text)) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "write",
    }
    // Half-close, then wait for the server's close: a closed client socket
    // sits orphaned in FIN_WAIT_2 and macOS resets it after a timeout.
    let _ = shutdown(c, 1);
    let _ = read_to_end(c);
    let _ = close(c);
    return 0;
}

fn accept_one(Stream listener) -> Stream {
    return match accept_wait(listener) {
        Result::Ok(c) => c,
        Result::Err(_) => panic "accept",
    };
}

fn body_len(Stream c) -> int {
    let n = match read_to_end(c) {
        Result::Ok(b) => len(b),
        Result::Err(_) => -1,
    };
    let _ = close(c);
    return n;
}

fn send_late(int port) -> int {
    return send_later(port, 30, "late data");
}

gen fn read_all(Stream c) -> int {
    yield body_len(c);
    return -1;
}

test("IO inside a generator without tasks parks the VM until data arrives") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let peer = spawn(send_late, port)?;
    let g = read_all(accept_one(listener));
    let n = resume g;
    join(peer)?;
    assert(n == 9)?;
}
