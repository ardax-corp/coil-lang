// Diagnostic copy of task_io.hy "connects from many tasks finish while the
// server task accepts": prints every result instead of panicking.
use task::{scope, Scope, TaskError};
use io::close;
use io::Stream;
use io::stdout;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use io::sync::accept_wait;
use io::sync::read_to_end;
use io::sync::write_all;
use string::{format, to_bytes};

fn say(string s) {
    let _ = write_all(stdout(), to_bytes(s));
}

fn client(int port, int k) -> int {
    let c = match connect("127.0.0.1", port) {
        Result::Ok(c) => c,
        Result::Err(e) => {
            say(format("client %i: connect err %s\n", k, format("%v", e)));
            return -10;
        },
    };
    match write_all(c, to_bytes("abc")) {
        Result::Ok(_) => {},
        Result::Err(e) => {
            say(format("client %i: write err %s\n", k, format("%v", e)));
            return -20;
        },
    }
    let _ = close(c);
    return 0;
}

fn server_one(Stream listener, int i) -> int {
    let c = match accept_wait(listener) {
        Result::Ok(c) => c,
        Result::Err(e) => {
            say(format("server %i: accept err %s\n", i, format("%v", e)));
            return -1000;
        },
    };
    let n = match read_to_end(c) {
        Result::Ok(b) => len(b),
        Result::Err(e) => {
            say(format("server %i: read err %s\n", i, format("%v", e)));
            -1
        },
    };
    let _ = close(c);
    if n != 3 {
        say(format("server %i: body len %i\n", i, n));
    }
    return n;
}

fn round(int r) -> int {
    let listener = match listen("127.0.0.1", 0) {
        Result::Ok(l) => l,
        Result::Err(_) => panic "listen",
    };
    let port = match local_addr(listener) {
        Result::Ok(a) => a[1],
        Result::Err(_) => panic "addr",
    };
    let res = scope(
        fn (Scope s) use (listener, port, r) {
            let server = s
                .spawn(
                    fn () use (listener) {
                        let total = 0;
                        let i = 0;
                        while i < 8 {
                            total = total + server_one(listener, i);
                            i = i + 1;
                        }
                        total
                    },
                );
            let i = 0;
            while i < 8 {
                let k = i;
                s.spawn(fn () use (port, k) => client(port, k));
                i = i + 1;
            }
            match server.join() {
                Result::Ok(v) => v,
                Result::Err(e) => {
                    say(format("round %i: join err %s\n", r, format("%v", e)));
                    -100
                },
            }
        },
    );
    let v = match res {
        Result::Ok(v) => v,
        Result::Err(e) => {
            say(format("round %i: scope err %s\n", r, format("%v", e)));
            -1
        },
    };
    say(format("round %i: total %i\n", r, v));
    return v;
}

fn main() {
    let bad = 0;
    let r = 0;
    while r < 5 {
        if round(r) != 24 {
            bad = bad + 1;
        }
        r = r + 1;
    }
    say(format("bad rounds: %i\n", bad));
}
