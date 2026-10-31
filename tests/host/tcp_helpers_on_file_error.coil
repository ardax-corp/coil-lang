// @needs sockets
// @output 11112
use io::open;
use io::stdout;
use io::write;
use io::net::tcp::accept;
use io::net::tcp::connect_timeout;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use io::net::tcp::set_nodelay;
use io::net::tcp::shutdown;
use string::{format, to_bytes};

fn main() {
    let path = "coil_io_timeout_tcp_helpers.bin";
    let file = open(path, "w")?;

    let local_on_file = match local_addr(file) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let nodelay_on_file = match set_nodelay(file, true) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let shutdown_on_file = match shutdown(file, 2) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };

    let listener = listen("127.0.0.1", 0)?;
    let addr = local_addr(listener)?;
    let accept_code = match accept(listener) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let connect_code = match connect_timeout("127.0.0.1", 1, 1) {
        Result::Ok(_) => 2,
        Result::Err(_) => 2,
    };

    let _ = write(
        stdout(),
        to_bytes(
            format(
                "%i%i%i%i%i",
                local_on_file,
                nodelay_on_file,
                shutdown_on_file,
                accept_code,
                connect_code,
            ),
        ),
    );
}
