// Doc stubs for the builtin `io::net::tcp` module (hover docs; never compiled).

/// Accepts one pending connection on `listener` without waiting.
///
/// Returns the connected stream, or `Err(IoError::WouldBlock)` when no
/// connection is waiting; then call `wait_readable(listener)` and retry,
/// or use `io::sync::accept_wait`, which does both. Returns
/// `Err(IoError::InvalidInput)` when `listener` is not a stream from
/// `listen`. Needs no permission flag (the grant is checked at `listen`).
///
/// # Examples
///
/// ```
/// use io::{wait_readable, IoError};
/// use io::net::tcp::{accept, connect, listen, local_addr};
/// # use tests::assert;
/// fn run() -> Result<bool, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let empty = match accept(listener) {
///         Result::Err(IoError::WouldBlock) => true,
///         default => false,
///     };
///     let (_, port) = local_addr(listener)?;
///     let _client = connect("127.0.0.1", port)?;
///     wait_readable(listener)?;
///     let _server = accept(listener)?;
///     return empty;
/// }
///
/// fn main() {
///     let ok = match run() {
///         Result::Ok(b) => b,
///         Result::Err(_) => false,
///     };
///     assert!(ok);
/// }
/// ```
fn accept(Stream listener) -> Result<Stream, IoError> {}

/// Opens a TCP connection to `host` on `port`.
///
/// `host` is an IP address (IPv6 may be written in brackets, `"[::1]"`) or
/// a name to resolve; every resolved address is tried in order. Waits
/// until the connection is made or fails; inside a task only that task
/// waits. Same as `connect_timeout(host, port, 0)`. The returned stream
/// is non-blocking like every `Stream`.
///
/// Returns `Err(IoError::InvalidInput)` for a port outside `0..=65535`,
/// `Err(IoError::NotFound)` when the name does not resolve, and
/// `Err(IoError::Other)` for errors such as a refused connection.
/// A program that reaches `connect` compiles only with `--allow-net`.
///
/// # Examples
///
/// ```
/// use io::{close, IoError};
/// use io::net::tcp::{connect, listen, local_addr, peer_addr};
/// # use tests::assert;
/// fn run() -> Result<int, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     let (_, peer_port) = peer_addr(client)?;
///     close(client)?;
///     return peer_port - port;
/// }
///
/// fn main() {
///     let diff = match run() {
///         Result::Ok(d) => d,
///         Result::Err(_) => -1,
///     };
///     assert!(diff == 0);
/// }
/// ```
fn connect(string host, int port) -> Result<Stream, IoError> {}

/// Opens a TCP connection to `host` on `port`, giving up after `ms` milliseconds.
///
/// Like `connect`, but name lookup and every address tried share one
/// deadline of `ms` milliseconds; when it passes the result is
/// `Err(IoError::TimedOut)`. `ms <= 0` waits forever.
/// Needs `--allow-net`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::net::tcp::{connect_timeout, listen, local_addr};
/// # use tests::assert;
/// fn run() -> Result<unit, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let _client = connect_timeout("127.0.0.1", port, 1000)?;
///     return ();
/// }
///
/// fn main() {
///     let ok = match run() {
///         Result::Ok(_) => true,
///         Result::Err(_) => false,
///     };
///     assert!(ok);
/// }
/// ```
fn connect_timeout(string host, int port, int ms) -> Result<Stream, IoError> {}

/// Opens a TCP socket listening on `host` and `port`.
///
/// `port` may be `0` to let the OS pick a free port; read it back with
/// `local_addr`. A host name is resolved and only the first address is
/// used, so prefer an explicit IP such as `"127.0.0.1"` or `"0.0.0.0"`.
/// Take connections with `accept` (or `io::sync::accept_wait`).
///
/// Returns `Err(IoError::InvalidInput)` for a port outside `0..=65535`;
/// an address already in use is reported as an OS error.
/// A program that reaches `listen` compiles only with `--allow-net`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::net::tcp::{connect, listen, local_addr};
/// use io::sync::{accept_wait, read_to_end, write_all};
/// use io::net::tcp::{shutdown};
/// use string::{from_bytes, to_bytes};
/// # use tests::assert;
/// fn run() -> Result<string, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     let server = accept_wait(listener)?;
///     write_all(client, to_bytes("hello"))?;
///     shutdown(client, 1)?;
///     return from_bytes(read_to_end(server)?)?;
/// }
///
/// fn main() {
///     let got = match run() {
///         Result::Ok(s) => s,
///         Result::Err(_) => "",
///     };
///     assert!(got == "hello");
/// }
/// ```
fn listen(string host, int port) -> Result<Stream, IoError> {}

/// Returns the local `(host, port)` of a TCP listener or connected TCP stream.
///
/// The host is an IP address; IPv6 addresses come in brackets
/// (`"[::1]"`). Useful after `listen(host, 0)` to learn the port the OS
/// picked. Returns `Err(IoError::InvalidInput)` for a stream that is not
/// TCP and `Err(IoError::AlreadyClosed)` once it is closed.
///
/// # Examples
///
/// ```
/// use io::net::tcp::{listen, local_addr};
/// # use tests::assert;
/// let listener = match listen("127.0.0.1", 0) {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "listen failed",
/// };
/// let (host, port) = match local_addr(listener) {
///     Result::Ok(addr) => addr,
///     Result::Err(_) => ("", 0),
/// };
/// assert!(host == "127.0.0.1" && port > 0);
/// ```
fn local_addr(Stream s) -> Result<(string, int), IoError> {}

/// Returns the remote `(host, port)` of a connected TCP stream.
///
/// The host is an IP address; IPv6 addresses come in brackets
/// (`"[::1]"`). Returns `Err(IoError::InvalidInput)` for a listener or a
/// stream that is not TCP.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::net::tcp::{connect, listen, local_addr, peer_addr};
/// # use tests::assert;
/// fn run() -> Result<(string, int), IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     return peer_addr(client)?;
/// }
///
/// fn main() {
///     let (host, _) = match run() {
///         Result::Ok(addr) => addr,
///         Result::Err(_) => ("", 0),
///     };
///     assert!(host == "127.0.0.1");
/// }
/// ```
fn peer_addr(Stream s) -> Result<(string, int), IoError> {}

/// Turns `TCP_NODELAY` on or off for a TCP stream.
///
/// With `enabled == true` small writes are sent at once instead of being
/// held back to be combined (Nagle's algorithm). Works on streams that a
/// package such as coil-tls has attached to, since they share the socket.
/// Returns `Err(IoError::InvalidInput)` for a listener or a stream that is
/// not TCP.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::net::tcp::{connect, listen, local_addr, set_nodelay};
/// # use tests::assert;
/// fn run() -> Result<unit, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     set_nodelay(client, true)?;
///     return ();
/// }
///
/// fn main() {
///     let ok = match run() {
///         Result::Ok(_) => true,
///         Result::Err(_) => false,
///     };
///     assert!(ok);
/// }
/// ```
fn set_nodelay(Stream s, bool enabled) -> Result<unit, IoError> {}

/// Shuts down the reading half, the writing half, or both halves of a TCP stream.
///
/// `how` is `0` for reading, `1` for writing and `2` for both. After
/// shutting down writing, the peer reads end of file. The stream stays
/// open until `close`. Any other `how` returns `Err(IoError::InvalidInput)`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::net::tcp::{connect, listen, local_addr, shutdown};
/// use io::sync::{accept_wait, read_to_end};
/// # use tests::assert;
/// fn run() -> Result<int, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     let server = accept_wait(listener)?;
///     shutdown(client, 1)?;
///     let rest = read_to_end(server)?;
///     return rest.len();
/// }
///
/// fn main() {
///     let n = match run() {
///         Result::Ok(n) => n,
///         Result::Err(_) => -1,
///     };
///     assert!(n == 0);
/// }
/// ```
fn shutdown(Stream s, int how) -> Result<unit, IoError> {}
