// `Option::None` in a two-slot Result CALL's arguments must stay a niche 0.
// Pair-emitting it under `unbox_enum_context` shifts the frame so `Stream.fd`
// sees a string (InvalidInput). Same shape as tls::client::enable → create_client.
use io::Stream;
use io::IoError;
use io::net::tcp::listen;

fn fd_with_opt(Stream s, string host, bool verify, Option<string> ca) -> Result<int, IoError> {
    let fd = s.fd()?;
    let _h = host;
    let _v = verify;
    let _c = ca;
    return fd;
}

fn make_ptr(Stream s, Option<string> ca) -> Result<int, IoError> {
    let fd = s.fd()?;
    let _c = ca;
    return fd;
}

fn enable_like(Stream s, Option<string> ca) -> Result<Stream, IoError> {
    let _ptr = make_ptr(s, ca)?;
    return s;
}

test("Stream.fd ? with Option arg in two-word Result") {
    let l = listen("127.0.0.1", 0)?;
    let n = fd_with_opt(l, "localhost", false, Option::None)?;
    assert(n >= 0)?;
}

test("two-word Result int ? into heap Result Stream") {
    let l = listen("127.0.0.1", 0)?;
    enable_like(l, Option::None)?;
}
