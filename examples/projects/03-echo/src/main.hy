// 03-echo — single-process TCP echo, client and server as tasks.
//
// Modules: protocol (framing), server/client (pure helpers).
// `task::scope` runs the server and the client as two tasks on one thread:
// while one waits for the socket, the other runs.
//
// Expected output: ok

use io::close;
use io::stdout;
use io::Stream;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use io::sync::accept_wait;
use io::sync::read_exact;
use io::sync::write_all;

use protocol::{encode_frame, frame_len, payload_eq};

use server::echo_reply;

use client::request_body;

use string::to_bytes;

use task::{scope, Scope, Task, TaskError};

// Read one length-prefixed frame (`[n][payload…]`).
fn read_frame(Stream s) -> Result<Vec<byte>, IoError> {
    let z: byte = 0;
    let head = Vec::from([z]);
    read_exact(s, head)?;
    let frame: Vec<byte> = Vec::new();
    frame.push(head[0]);
    let n = frame_len(frame);
    let i = 0;
    while i < n {
        let b = Vec::from([z]);
        read_exact(s, b)?;
        frame.push(b[0]);
        i = i + 1;
    }
    return Result::Ok(frame);
}

// Accept one connection and echo one frame back.
fn serve_one(Stream listener) -> Result<int, IoError> {
    let conn = accept_wait(listener)?;
    let frame = read_frame(conn)?;
    write_all(conn, echo_reply(frame))?;
    close(conn)?;
    return Result::Ok(0);
}

// Send the request frame and return the echoed frame.
fn ask(int port, Vec<byte> body) -> Result<Vec<byte>, IoError> {
    let conn = connect("127.0.0.1", port)?;
    write_all(conn, encode_frame(body))?;
    let back = read_frame(conn)?;
    close(conn)?;
    return Result::Ok(back);
}

fn served(Task<Result<int, IoError>> t) -> bool {
    return match t.join() {
        Result::Ok(Result::Ok(_)) => true,
        default => false,
    };
}

fn run_echo() -> Result<string, IoError> {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let body = request_body();
    let r = scope(
        fn (Scope s) use (listener, port, body) {
            let server = s.spawn(fn () use (listener) => serve_one(listener));
            let client = s.spawn(fn () use (port, body) => ask(port, body));
            let back = match client.join() {
                Result::Ok(Result::Ok(frame)) => frame,
                default => Vec::new(),
            };
            served(server) && payload_eq(back, body) == 1
        },
    );
    close(listener)?;
    return match r {
        Result::Ok(true) => Result::Ok("ok"),
        default => Result::Ok("bad"),
    };
}

fn main() {
    let text = match run_echo() {
        Result::Ok(s) => s,
        Result::Err(_) => "err",
    };
    write_all(stdout(), to_bytes(text));
}
