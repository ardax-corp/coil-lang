// 03-echo — single-process TCP echo (listen + connect + exchange).
//
// Modules: protocol (framing), server/client (pure helpers).
// Stream IO is in this entry file for clarity (deps may also call IO + `?`).
//
// Expected output: ok

use io::close;
use io::stdout;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::sync::accept_wait;
use io::sync::read_exact;
use io::sync::write_all;

use protocol::{encode_frame, payload_eq};

use server::echo_reply;

use client::{client_port, request_body};

use string::{format, to_bytes};

async fn greeting_bytes() {
    yield 65;
    yield 66;
    return 0;
}

fn run_echo() {
    let port = client_port();
    let listener = listen("127.0.0.1", port)?;
    let client = connect("127.0.0.1", port)?;
    let server = accept_wait(listener)?;

    let h = greeting_bytes();
    let ya = resume h;
    let yb = resume h;
    let _done = resume h;
    if ya != 65 {
        close(client)?;
        close(server)?;
        close(listener)?;
        return "bad-coro";
    }
    if yb != 66 {
        close(client)?;
        close(server)?;
        close(listener)?;
        return "bad-coro";
    }

    let body = request_body();
    let frame = encode_frame(body);
    write_all(client, frame)?;

    let z: byte = 0;
    let s0 = Vec::from([z]);
    let s1 = Vec::from([z]);
    let s2 = Vec::from([z]);
    read_exact(server, s0)?;
    read_exact(server, s1)?;
    read_exact(server, s2)?;

    let inbound: Vec<byte> = Vec::new();
    inbound.push(s0[0]);
    inbound.push(s1[0]);
    inbound.push(s2[0]);
    let reply = echo_reply(inbound);
    write_all(server, reply)?;

    let c0 = Vec::from([z]);
    let c1 = Vec::from([z]);
    let c2 = Vec::from([z]);
    read_exact(client, c0)?;
    read_exact(client, c1)?;
    read_exact(client, c2)?;

    close(client)?;
    close(server)?;
    close(listener)?;

    let back: Vec<byte> = Vec::new();
    back.push(c0[0]);
    back.push(c1[0]);
    back.push(c2[0]);
    if payload_eq(back, body) == 1 {
        return "ok";
    }
    return "bad";
}

fn main() {
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match run_echo() {
                    Result::Ok(s) => s,
                    Result::Err(_) => "err",
                },
            ),
        ),
    );
}
