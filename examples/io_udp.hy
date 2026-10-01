// UDP datagram round-trip via `io::net::udp`.
// Server binds ephemeral port; client send_to; server recv_from_wait.
//
// Output: 2

use io::close;
use io::stdout;
use io::net::udp::bind;
use io::net::udp::local_port;
use io::net::udp::send_to;
use io::sync::recv_from_wait;
use io::sync::write_all;

use string::{format, to_bytes};

fn echo_once() {
    let server = bind("127.0.0.1", 0)?;
    let port = local_port(server)?;
    let client = bind("127.0.0.1", 0)?;
    let msg = to_bytes("Hi");
    send_to(client, msg, "127.0.0.1", port)?;
    let z: byte = 0;
    let buf = Vec::from([z, z, z, z, z, z, z, z]);
    let t = recv_from_wait(server, buf)?;
    close(server)?;
    close(client)?;
    return format("%i", t[0]);
}

fn main() {
    write_all(
        stdout(),
        to_bytes(
            format(
                "%s",
                match echo_once() {
                    Result::Ok(s) => s,
                    Result::Err(_) => "err",
                },
            ),
        ),
    );
}
