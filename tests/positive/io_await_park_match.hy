// COI-408: park on WouldBlock, resume, match Result::Ok (not boxed-as-Err).
use io::{await_readable, await_writable, close, read, write};
use io::net::tcp::{accept, connect, listen, local_addr};
use thread::{Sender, channel, join, recv, send, spawn};
use clock::{sleep_ms};
use string::{to_bytes};

fn delay_http_server(Sender tx) -> int {
    let listener = match listen("127.0.0.1", 0) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "listen",
    };
    let addr = match local_addr(listener) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "local_addr",
    };
    match send(tx, addr[1]) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "send port",
    };
    match await_readable(listener) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "listen wait",
    };
    let peer = match accept(listener) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "accept",
    };
    sleep_ms(80);
    match write(peer, to_bytes("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "server write",
    };
    match close(peer) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close peer",
    };
    match close(listener) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close listener",
    };
    return 0;
}

fn delay_drain_server(Sender tx) -> int {
    let listener = match listen("127.0.0.1", 0) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "listen",
    };
    let addr = match local_addr(listener) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "local_addr",
    };
    match send(tx, addr[1]) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "send port",
    };
    match await_readable(listener) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "listen wait",
    };
    let peer = match accept(listener) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "accept",
    };
    sleep_ms(80);
    let z: byte = 0;
    let buf = Vec::from([z, z, z, z, z, z, z, z]);
    let n = 0;
    while n < 512 {
        match read(peer, buf) {
            Result::Ok(got) => {
                match got {
                    Option::None => {
                        break;
                    },
                    Option::Some(_) => {},
                };
            },
            Result::Err(_) => {
                match await_readable(peer) {
                    Result::Ok(_) => {},
                    Result::Err(_) => panic "drain wait",
                };
            },
        };
        n = n + 1;
    };
    match close(peer) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close peer",
    };
    match close(listener) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close listener",
    };
    return 0;
}

test("await_readable match Ok after WouldBlock park") {
    let pair = match channel() {
        Result::Ok(v) => v,
        Result::Err(_) => panic "channel",
    };
    let t = match spawn(delay_http_server, pair[0]) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "spawn",
    };
    let port = match recv(pair[1]) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "recv port",
    };
    let c = match connect("127.0.0.1", port) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "connect",
    };
    match write(c, to_bytes("GET / HTTP/1.1\r\nHost: t\r\n\r\n")) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "client write",
    };
    let z: byte = 0;
    let buf = Vec::from([z, z, z, z, z, z, z, z]);
    match read(c, buf) {
        Result::Ok(_) => panic "first read should WouldBlock",
        Result::Err(_) => {
            match await_readable(c) {
                Result::Ok(_) => {},
                Result::Err(_) => panic "await_readable treated Ok as Err",
            };
        },
    };
    match read(c, buf) {
        Result::Ok(got) => {
            match got {
                Option::Some(n) => assert(n > 0)?,
                Option::None => panic "eof before body",
            };
        },
        Result::Err(_) => panic "second read after wait",
    };
    match close(c) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close client",
    };
    match join(t) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "join",
    };
}

test("await_writable match Ok after WouldBlock park") {
    let pair = match channel() {
        Result::Ok(v) => v,
        Result::Err(_) => panic "channel",
    };
    let t = match spawn(delay_drain_server, pair[0]) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "spawn",
    };
    let port = match recv(pair[1]) {
        Result::Ok(v) => v,
        Result::Err(_) => panic "recv port",
    };
    let c = match connect("127.0.0.1", port) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "connect",
    };
    let one: byte = 1;
    let chunk: Vec<byte> = Vec::new();
    let i = 0;
    while i < 4096 {
        chunk.push(one);
        i = i + 1;
    };
    let blocked = 0;
    let n = 0;
    while n < 512 {
        match write(c, chunk) {
            Result::Ok(_) => {},
            Result::Err(_) => {
                blocked = 1;
                break;
            },
        };
        n = n + 1;
    };
    assert(blocked == 1)?;
    match await_writable(c) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "await_writable treated Ok as Err",
    };
    match close(c) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "close client",
    };
    match join(t) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "join",
    };
}
