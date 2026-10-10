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

test("an IO wait in one task lets another task run") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let log: Vec<string> = Vec::new();
    let r = scope(
        fn (Scope s) use (listener, port, log) {
            let server = s
                .spawn(
                    fn () use (listener, log) {
                        let conn = accept_one(listener);
                        log.push("accepted");
                        body_len(conn)
                    },
                );
            s
                .spawn(
                    fn () use (port, log) {
                        task::sleep(20);
                        log.push("connecting");
                        send_later(port, 0, "hello tasks")
                    },
                );
            ok_or(server.join(), -100)
        },
    );
    assert(ok_or(r, -1) == 11)?;
    assert(log.len() == 2 && log[0] == "connecting" && log[1] == "accepted")?;
}

test("IO inside a generator suspends the task, not the generator") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let r = scope(
        fn (Scope s) use (listener, port) {
            let reader = s
                .spawn(
                    fn () use (listener) {
                        let g = read_all(accept_one(listener));
                        // The read parks inside the generator; the first value it yields
                        // is the real length, not a placeholder.
                        resume g
                    },
                );
            s.spawn(fn () use (port) => send_later(port, 20, "twelve bytes"));
            ok_or(reader.join(), -100)
        },
    );
    assert(ok_or(r, -1) == 12)?;
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

test("connects from many tasks finish while the server task accepts") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let r = scope(
        fn (Scope s) use (listener, port) {
            let server = s
                .spawn(
                    fn () use (listener) {
                        let total = 0;
                        let i = 0;
                        while i < 8 {
                            total = total + body_len(accept_one(listener));
                            i = i + 1;
                        }
                        total
                    },
                );
            let i = 0;
            while i < 8 {
                s.spawn(fn () use (port) => send_later(port, 0, "abc"));
                i = i + 1;
            }
            ok_or(server.join(), -100)
        },
    );
    assert(ok_or(r, -1) == 24)?;
}
