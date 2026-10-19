// Doc stubs for the builtin `io::net::udp` module (hover docs; never compiled).

/// Opens a UDP socket bound to `host` and `port`.
///
/// `port` may be `0` to let the OS pick a free port; read it back with
/// `local_port`. A host name is resolved and only the first address is
/// used, so prefer an explicit IP such as `"127.0.0.1"` or `"0.0.0.0"`.
/// Exchange datagrams with `send_to` and `recv_from`.
///
/// Returns `Err(IoError::InvalidInput)` for a port outside `0..=65535`.
/// A program that reaches `bind` compiles only with `--allow-net`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::net::udp::{bind, local_port};
/// # use tests::assert;
/// fn run() -> Result<int, IoError> {
///     let sock = bind("127.0.0.1", 0)?;
///     return local_port(sock)?;
/// }
///
/// fn main() {
///     let port = match run() {
///         Result::Ok(p) => p,
///         Result::Err(_) => 0,
///     };
///     assert!(port > 0);
/// }
/// ```
fn bind(string host, int port) -> Result<Stream, IoError> {}

/// Opens a UDP socket whose default peer is `host` on `port`.
///
/// The socket is bound to an ephemeral IPv4 port (`0.0.0.0:0`), so the
/// peer must be an IPv4 address. Afterwards `io::write` sends a datagram
/// to that peer and `io::read` receives one from it. Nothing is sent by
/// `connect` itself.
/// A program that reaches `connect` compiles only with `--allow-net`.
///
/// # Examples
///
/// ```
/// use io::{write, IoError};
/// use io::net::udp::{bind, connect, local_port};
/// use io::sync::{recv_from_wait};
/// use string::{to_bytes};
/// # use tests::assert;
/// fn run() -> Result<int, IoError> {
///     let server = bind("127.0.0.1", 0)?;
///     let port = local_port(server)?;
///     let client = connect("127.0.0.1", port)?;
///     write(client, to_bytes("ping"))?;
///     let (n, _, _) = recv_from_wait(server, to_bytes("........"))?;
///     return n;
/// }
///
/// fn main() {
///     let n = match run() {
///         Result::Ok(n) => n,
///         Result::Err(_) => 0,
///     };
///     assert!(n == 4);
/// }
/// ```
fn connect(string host, int port) -> Result<Stream, IoError> {}

/// Returns the local port a UDP socket is bound to.
///
/// Useful after `bind(host, 0)` or `connect` to learn the port the OS
/// picked. Returns `Err(IoError::AlreadyClosed)` when `s` is not an open
/// UDP socket.
///
/// # Examples
///
/// ```
/// use io::net::udp::{bind, local_port};
/// # use tests::assert;
/// let sock = match bind("127.0.0.1", 0) {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "bind failed",
/// };
/// let port = match local_port(sock) {
///     Result::Ok(p) => p,
///     Result::Err(_) => 0,
/// };
/// assert!(port > 0 && port <= 65535);
/// ```
fn local_port(Stream s) -> Result<int, IoError> {}

/// Receives one datagram into the front of `buf` without waiting.
///
/// Returns `(n, host, port)`: the number of bytes written to `buf[0..n]`
/// and the sender's IP address (IPv6 without brackets) and port. A
/// datagram longer than `buf.len()` is cut to fit; the rest is lost.
/// Returns `Err(IoError::WouldBlock)` when no datagram is waiting; then
/// call `wait_readable(s)` and retry, or use `io::sync::recv_from_wait`.
/// An empty `buf` returns `(0, "", 0)` at once.
/// Needs no permission flag (the grant is checked at `bind`).
///
/// # Examples
///
/// ```
/// use io::{wait_readable, IoError};
/// use io::net::udp::{bind, local_port, recv_from, send_to};
/// use string::{to_bytes};
/// # use tests::assert;
/// fn run() -> Result<(int, string, int), IoError> {
///     let server = bind("127.0.0.1", 0)?;
///     let port = local_port(server)?;
///     let client = bind("127.0.0.1", 0)?;
///     send_to(client, to_bytes("hi"), "127.0.0.1", port)?;
///     wait_readable(server)?;
///     return recv_from(server, to_bytes("........"))?;
/// }
///
/// fn main() {
///     let (n, host, port) = match run() {
///         Result::Ok(t) => t,
///         Result::Err(_) => (0, "", 0),
///     };
///     assert!(n == 2 && host == "127.0.0.1" && port > 0);
/// }
/// ```
fn recv_from(Stream s, Vec<byte> buf) -> Result<(int, string, int), IoError> {}

/// Sends `buf` as one datagram to `host` on `port` and returns how many bytes were sent.
///
/// Does not wait: returns `Err(IoError::WouldBlock)` when the OS send
/// buffer is full (`io::sync::send_to_wait` waits and retries). `host` is
/// resolved like in `bind`. Returns `Err(IoError::InvalidInput)` for a port
/// outside `0..=65535`.
/// A program that reaches `send_to` compiles only with `--allow-net`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::net::udp::{bind, local_port, send_to};
/// use string::{to_bytes};
/// # use tests::assert;
/// fn run() -> Result<int, IoError> {
///     let server = bind("127.0.0.1", 0)?;
///     let port = local_port(server)?;
///     let client = bind("127.0.0.1", 0)?;
///     return send_to(client, to_bytes("hello"), "127.0.0.1", port)?;
/// }
///
/// fn main() {
///     let sent = match run() {
///         Result::Ok(n) => n,
///         Result::Err(_) => 0,
///     };
///     assert!(sent == 5);
/// }
/// ```
fn send_to(Stream s, Vec<byte> buf, string host, int port) -> Result<int, IoError> {}
