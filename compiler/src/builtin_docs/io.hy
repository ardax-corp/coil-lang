// Doc stubs for the builtin `io` module (hover docs; never compiled).

/// An open file, socket or standard stream.
///
/// A `Stream` is an opaque heap handle made by `open`, `stdin` / `stdout` /
/// `stderr`, or the socket functions in `io::net::tcp` and `io::net::udp`.
/// Every stream is non-blocking: `read`, `write` and the socket calls never
/// wait, they return `Err(IoError::WouldBlock)` when the OS is not ready.
/// Use `wait_readable` / `wait_writable` to wait, or the blocking adapters in
/// `io::sync` (`write_all`, `read_to_end`, ...).
///
/// The handle is closed by `close`, or when the garbage collector drops it.
/// `Stream` implements `Read` and `Write`, so `s.read(buf)` and
/// `s.write(buf)` work as methods too.
///
/// # Examples
///
/// ```
/// use io::{close, open, Stream};
/// use io::fs::{remove_file};
/// use io::sync::{read_to_end, write_all};
/// use string::{from_bytes, to_bytes};
/// # use tests::assert;
/// let path = "/tmp/coil_doc_stream.txt";
/// let s: Stream = match open(path, "w") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let _ = write_all(s, to_bytes("hello"));
/// let _ = close(s);
///
/// let r = match open(path, "r") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let text = match read_to_end(r) {
///     Result::Ok(bytes) => match from_bytes(bytes) {
///         Result::Ok(t) => t,
///         Result::Err(_) => "",
///     },
///     Result::Err(_) => "",
/// };
/// let _ = close(r);
/// let _ = remove_file(path);
/// assert!(text == "hello");
/// ```
class Stream {}

impl Stream {
    /// Installs package IO hooks on this stream in place and returns the same stream.
    ///
    /// `ptr` is the package's session pointer and `read`, `write`,
    /// `shutdown` and `free` are C function pointers (as `int`, usually
    /// looked up through `dload`). After `attach`, `read` / `write` on the
    /// stream go through those hooks, `close` (or a GC drop) calls `shutdown`
    /// then `free`, and the socket keeps its descriptor. This is how the
    /// coil-tls package turns a TCP stream into a TLS stream.
    ///
    /// Returns `Err(IoError::InvalidInput)` when `ptr` is `0` or the stream is
    /// already attached, and `Err(IoError::AlreadyClosed)` on a closed stream.
    /// A program that reaches `attach` compiles only with `--allow-attach`.
    ///
    /// # Examples
    ///
    /// ```text
    /// use io::net::tcp::{connect};
    /// let s = connect("example.com", 443)?;
    /// // session, rd, wr, sd, fr: a session pointer and C hooks from a dload'ed library
    /// let tls = s.attach(session, rd, wr, sd, fr)?;
    /// ```
    pub fn attach(int ptr, int read, int write, int shutdown, int free) -> Result<Stream, IoError> {}

    /// Waits until this stream's descriptor is ready.
    ///
    /// Waits for readability, or for writability when an attached package's
    /// last write stopped with `WouldBlock` (for example a TLS handshake that
    /// still has data to flush). Inside a task only that task waits. Meant
    /// for packages built on `attach`; plain code uses `wait_readable` /
    /// `wait_writable`. Returns `Err(IoError::AlreadyClosed)` on a closed
    /// stream and `Err(IoError::TimedOut)` when the stream's timeout passes.
    ///
    /// # Examples
    ///
    /// ```
    /// use io::{read, IoError};
    /// use io::net::tcp::{connect, listen, local_addr};
    /// use io::sync::{accept_wait, write_all};
    /// use string::{to_bytes};
    /// # use tests::assert;
    /// fn run() -> Result<Option<int>, IoError> {
    ///     let listener = listen("127.0.0.1", 0)?;
    ///     let (_, port) = local_addr(listener)?;
    ///     let client = connect("127.0.0.1", port)?;
    ///     let server = accept_wait(listener)?;
    ///     write_all(client, to_bytes("hi"))?;
    ///     server.park()?;
    ///     return read(server, to_bytes("....."))?;
    /// }
    ///
    /// fn main() {
    ///     let n = match run() {
    ///         Result::Ok(Option::Some(n)) => n,
    ///         default => 0,
    ///     };
    ///     assert!(n == 2);
    /// }
    /// ```
    pub fn park() -> Result<unit, IoError> {}

    /// Returns the OS file descriptor (socket handle on Windows) of this stream.
    ///
    /// The descriptor stays owned by the stream: do not close it yourself.
    /// `stdin`, `stdout` and `stderr` return duplicated descriptors, so their
    /// `fd` is not `0`, `1` or `2`. Returns `Err(IoError::InvalidInput)` once
    /// the stream is closed.
    ///
    /// # Examples
    ///
    /// ```
    /// use io::{stdout};
    /// # use tests::assert;
    /// let fd = match stdout().fd() {
    ///     Result::Ok(fd) => fd,
    ///     Result::Err(_) => -1,
    /// };
    /// assert!(fd >= 0);
    /// ```
    pub fn fd() -> Result<int, IoError> {}
}

/// The error type of every `io`, `io::fs` and `io::net` operation.
///
/// OS errors map to the closest variant; anything without one is `Other`.
/// `IoError` implements `Show` (the variant name), so `%v` in `format`
/// prints it.
///
/// # Examples
///
/// ```
/// use io::{open, IoError};
/// use string::{format};
/// # use tests::assert;
/// let msg = match open("/tmp/coil_doc_no_such_dir/file.txt", "r") {
///     Result::Ok(_) => "opened",
///     Result::Err(IoError::NotFound) => "missing",
///     Result::Err(_) => "other",
/// };
/// assert!(msg == "missing");
/// assert!(format("%v", IoError::TimedOut) == "TimedOut");
/// ```
enum IoError {
    /// The operation is not ready yet and would have to wait; retry after `wait_readable` / `wait_writable`.
    WouldBlock,
    /// The file, directory or host was not found.
    NotFound,
    /// The OS refused the operation.
    PermissionDenied,
    /// The stream was already closed.
    AlreadyClosed,
    /// An argument was invalid: bad open mode, port out of range, bad UTF-8, wrong kind of stream, ...
    InvalidInput,
    /// Any other OS error.
    Other,
    /// A path component is not a directory.
    NotADirectory,
    /// The file or directory already exists.
    AlreadyExists,
    /// A deadline or OS timeout passed (distinct from `WouldBlock`).
    TimedOut,
    /// The data ended early (the OS reported an unexpected end of file).
    Truncated,
    /// A TLS certificate was rejected (reported by attached packages such as coil-tls).
    Certificate,
    /// A TLS handshake failed (reported by attached packages such as coil-tls).
    Handshake,
}

/// Types that bytes can be read from; implemented for `Stream`.
///
/// `read` behaves like the free function `io::read`.
///
/// # Examples
///
/// ```
/// use io::{close, open, Read};
/// use io::fs::{remove_file};
/// use io::sync::{write_all};
/// use string::{to_bytes};
/// # use tests::assert;
/// let path = "/tmp/coil_doc_read_trait.txt";
/// let w = match open(path, "w") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let _ = write_all(w, to_bytes("abc"));
/// let _ = close(w);
/// let r = match open(path, "r") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let n = match r.read(to_bytes("........")) {
///     Result::Ok(Option::Some(n)) => n,
///     default => 0,
/// };
/// let _ = close(r);
/// let _ = remove_file(path);
/// assert!(n == 3);
/// ```
trait Read<S> {
    /// Reads into `buf`; see `io::read`.
    fn read(S s, Vec<byte> buf) -> Result<Option<int>, IoError> {}
}

/// Types that bytes can be written to; implemented for `Stream`.
///
/// `write` behaves like the free function `io::write`.
///
/// # Examples
///
/// ```
/// use io::{stdout, Write};
/// use string::{to_bytes};
/// # use tests::assert;
/// let n = match stdout().write(to_bytes("hi\n")) {
///     Result::Ok(n) => n,
///     Result::Err(_) => 0,
/// };
/// assert!(n == 3);
/// ```
trait Write<S> {
    /// Writes from `buf`; see `io::write`.
    fn write(S s, Vec<byte> buf) -> Result<int, IoError> {}
}

/// Returns a new stream on the process's standard input.
///
/// The stream holds a duplicated descriptor, so closing it does not close
/// the process's stdin. Like every stream it is non-blocking: `read`
/// returns `Err(IoError::WouldBlock)` until input is available. Use
/// `io::sync::read_line` or `read_to_end` to wait for input.
///
/// # Examples
///
/// ```
/// use io::{close, stdin};
/// # use tests::assert;
/// let s = stdin();
/// let closed = match close(s) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// assert!(closed);
/// ```
fn stdin() -> Stream {}

/// Returns a new stream on the process's standard output.
///
/// The stream holds a duplicated descriptor, so closing it does not close
/// the process's stdout. `write` may write only part of the buffer; use
/// `io::sync::write_all` (or `io::sync::println`) to write everything.
///
/// # Examples
///
/// ```
/// use io::{stdout};
/// use io::sync::{write_all};
/// use string::{to_bytes};
/// let _ = write_all(stdout(), to_bytes("hello\n"));
/// ```
fn stdout() -> Stream {}

/// Returns a new stream on the process's standard error.
///
/// The stream holds a duplicated descriptor, so closing it does not close
/// the process's stderr.
///
/// # Examples
///
/// ```
/// use io::{stderr, write};
/// use string::{to_bytes};
/// # use tests::assert;
/// let n = match write(stderr(), to_bytes("")) {
///     Result::Ok(n) => n,
///     Result::Err(_) => -1,
/// };
/// assert!(n == 0);
/// ```
fn stderr() -> Stream {}

/// Starts keeping this thread's stdout and stderr writes in memory instead
/// of writing them, until the matching `capture_end()`.
///
/// Captures nest: each `capture_end()` closes the innermost one. Use it in
/// tests to check what code prints. Threads spawned while capturing write
/// to their own output. A capture still open when a test case ends goes to
/// that case's output.
///
/// # Examples
///
/// ```
/// use io::{capture_begin, capture_end, stdout, write};
/// use string::{to_bytes};
/// # use tests::assert;
/// capture_begin();
/// let _ = write(stdout(), to_bytes("hi\n"));
/// let (out, err) = capture_end();
/// assert!(out == "hi\n" && err == "");
/// ```
fn capture_begin() {}

/// Ends the innermost `capture_begin()` and returns what it kept, as
/// `(stdout, stderr)`. Both are empty when no capture is open.
///
/// # Examples
///
/// ```
/// use io::{capture_begin, capture_end, stderr, write};
/// use string::{to_bytes};
/// # use tests::assert;
/// capture_begin();
/// let _ = write(stderr(), to_bytes("oops"));
/// let (_, err) = capture_end();
/// assert!(err == "oops");
/// ```
fn capture_end() -> (string, string) {}

/// Opens the file at `path` with `mode` and returns a stream on it.
///
/// Modes:
///
/// - `"r"`: read only; the file must exist.
/// - `"w"`: write only; creates the file or truncates it.
/// - `"a"`: append; creates the file if needed, writes go to the end.
/// - `"rw"`: read and write; creates the file if needed, does not truncate.
///
/// Any other mode returns `Err(IoError::InvalidInput)`. OS errors map to
/// `NotFound`, `PermissionDenied`, `NotADirectory` and so on.
///
/// A program that reaches `open` compiles only with the matching grant: a
/// literal `"r"` needs `--allow-read`, `"w"` and `"a"` need `--allow-write`,
/// `"rw"` and a mode computed at run time need both.
///
/// # Examples
///
/// ```
/// use io::{close, open};
/// use io::fs::{remove_file};
/// use io::sync::{read_to_end, write_all};
/// use string::{to_bytes};
/// # use tests::assert;
/// let path = "/tmp/coil_doc_open.txt";
/// let w = match open(path, "w") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let _ = write_all(w, to_bytes("ab"));
/// let _ = close(w);
/// let a = match open(path, "a") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let _ = write_all(a, to_bytes("cd"));
/// let _ = close(a);
/// let r = match open(path, "r") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let size = match read_to_end(r) {
///     Result::Ok(bytes) => bytes.len(),
///     Result::Err(_) => 0,
/// };
/// let _ = close(r);
/// let _ = remove_file(path);
/// assert!(size == 4);
/// ```
fn open(string path, string mode) -> Result<Stream, IoError> {}

/// Closes `s`.
///
/// Closing an attached stream calls its package `shutdown` and `free`
/// hooks first, and wakes any task waiting on it. Closing twice returns
/// `Err(IoError::AlreadyClosed)`. A stream that is never closed is closed
/// when the garbage collector drops it.
///
/// # Examples
///
/// ```
/// use io::{close, stdout, IoError};
/// # use tests::assert;
/// let s = stdout();
/// let first = match close(s) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let second = match close(s) {
///     Result::Err(IoError::AlreadyClosed) => true,
///     default => false,
/// };
/// assert!(first && second);
/// ```
fn close(Stream s) -> Result<unit, IoError> {}

/// Reads bytes from `s` into the front of `buf` without waiting.
///
/// Reads at most `buf.len()` bytes (the buffer's length, not its capacity),
/// overwriting `buf[0..n]`. Returns `Ok(Option::Some(n))` with the number of
/// bytes read, `Ok(Option::None)` at end of file, and
/// `Err(IoError::WouldBlock)` when no data is available yet; then call
/// `wait_readable(s)` and retry. An empty `buf` returns `Ok(Option::Some(0))`.
/// `io::sync::read_exact` and `read_to_end` do the waiting for you.
///
/// # Examples
///
/// ```
/// use io::{close, open, read};
/// use io::fs::{remove_file};
/// use io::sync::{write_all};
/// use string::{to_bytes};
/// # use tests::assert;
/// let path = "/tmp/coil_doc_read.txt";
/// let w = match open(path, "w") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let _ = write_all(w, to_bytes("hey"));
/// let _ = close(w);
///
/// let r = match open(path, "r") {
///     Result::Ok(s) => s,
///     Result::Err(_) => panic "open failed",
/// };
/// let buf = to_bytes("..........");
/// let first = match read(r, buf) {
///     Result::Ok(Option::Some(n)) => n,
///     default => 0,
/// };
/// let at_eof = match read(r, buf) {
///     Result::Ok(Option::None) => true,
///     default => false,
/// };
/// let _ = close(r);
/// let _ = remove_file(path);
/// assert!(first == 3);
/// assert!(buf[0] == 104 as byte);
/// assert!(at_eof);
/// ```
fn read(Stream s, Vec<byte> buf) -> Result<Option<int>, IoError> {}

/// Writes bytes from `buf` to `s` without waiting and returns how many were written.
///
/// The count may be less than `buf.len()` when the OS accepts only part of
/// it, and `Err(IoError::WouldBlock)` means nothing could be written yet;
/// call `wait_writable(s)` and retry. `io::sync::write_all` loops until
/// everything is written. Writing to a closed stream returns
/// `Err(IoError::AlreadyClosed)`.
///
/// # Examples
///
/// ```
/// use io::{stdout, write};
/// use string::{to_bytes};
/// # use tests::assert;
/// let n = match write(stdout(), to_bytes("hi\n")) {
///     Result::Ok(n) => n,
///     Result::Err(_) => 0,
/// };
/// assert!(n == 3);
/// ```
fn write(Stream s, Vec<byte> buf) -> Result<int, IoError> {}

/// Writes `buf[offset..]` to `s` without waiting and returns how many bytes were written.
///
/// Like `write`, but starts at `offset` without copying the rest of the
/// buffer, which suits a loop that resumes after a partial write.
/// `offset == buf.len()` writes nothing and returns `Ok(0)`; a negative
/// offset or one past the end returns `Err(IoError::InvalidInput)`.
///
/// # Examples
///
/// ```
/// use io::{stdout, write_from};
/// use string::{to_bytes};
/// # use tests::assert;
/// let buf = to_bytes("skip: shown\n");
/// let n = match write_from(stdout(), buf, 6) {
///     Result::Ok(n) => n,
///     Result::Err(_) => 0,
/// };
/// assert!(n == 6);
/// ```
fn write_from(Stream s, Vec<byte> buf, int offset) -> Result<int, IoError> {}

/// Waits until `s` is readable.
///
/// Returns at once when data (or end of file) is already available.
/// Inside a task only that task waits while the others run; elsewhere the
/// program waits. Returns `Err(IoError::TimedOut)` when the stream's read
/// timeout passes and `Err(IoError::AlreadyClosed)` on a closed stream.
/// `await_readable` is an older name for the same function.
///
/// # Examples
///
/// ```
/// use io::{read, wait_readable, IoError};
/// use io::net::tcp::{connect, listen, local_addr};
/// use io::sync::{accept_wait, write_all};
/// use string::{to_bytes};
/// # use tests::assert;
/// fn run() -> Result<Option<int>, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     let server = accept_wait(listener)?;
///     write_all(client, to_bytes("ping"))?;
///     wait_readable(server)?;
///     return read(server, to_bytes("........"))?;
/// }
///
/// fn main() {
///     let n = match run() {
///         Result::Ok(Option::Some(n)) => n,
///         default => 0,
///     };
///     assert!(n == 4);
/// }
/// ```
fn wait_readable(Stream s) -> Result<unit, IoError> {}

/// Waits until `s` is writable.
///
/// Returns at once when the stream can take more data. Inside a task only
/// that task waits while the others run; elsewhere the program waits.
/// Returns `Err(IoError::TimedOut)` when the stream's write timeout passes
/// and `Err(IoError::AlreadyClosed)` on a closed stream.
/// `await_writable` is an older name for the same function.
///
/// # Examples
///
/// ```
/// use io::{stdout, wait_writable};
/// # use tests::assert;
/// let ready = match wait_writable(stdout()) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// assert!(ready);
/// ```
fn wait_writable(Stream s) -> Result<unit, IoError> {}

/// Older name of `wait_readable`; behaves exactly the same.
///
/// # Examples
///
/// ```
/// use io::{await_readable, read, IoError};
/// use io::net::tcp::{connect, listen, local_addr};
/// use io::sync::{accept_wait, write_all};
/// use string::{to_bytes};
/// # use tests::assert;
/// fn run() -> Result<Option<int>, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     let server = accept_wait(listener)?;
///     write_all(client, to_bytes("x"))?;
///     await_readable(server)?;
///     return read(server, to_bytes("...."))?;
/// }
///
/// fn main() {
///     let n = match run() {
///         Result::Ok(Option::Some(n)) => n,
///         default => 0,
///     };
///     assert!(n == 1);
/// }
/// ```
fn await_readable(Stream s) -> Result<unit, IoError> {}

/// Older name of `wait_writable`; behaves exactly the same.
///
/// # Examples
///
/// ```
/// use io::{await_writable, stdout};
/// # use tests::assert;
/// let ready = match await_writable(stdout()) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// assert!(ready);
/// ```
fn await_writable(Stream s) -> Result<unit, IoError> {}

/// Deprecated: polls the registered IO waiters once and returns how many became ready.
///
/// Calling it gives warning `E0129`. Run concurrent IO as tasks instead
/// (`task::scope` with `spawn` and `join`): an IO wait inside a task
/// suspends only that task.
///
/// # Examples
///
/// ```text
/// use io::{drive};
/// let ready = drive(); // warning E0129: `drive` is deprecated
/// ```
fn drive() -> int {}

/// Deprecated: waits until any registered IO waiter is ready and returns how many became ready.
///
/// Calling it gives warning `E0129`. Run concurrent IO as tasks instead
/// (`task::scope` with `spawn` and `join`): an IO wait inside a task
/// suspends only that task.
///
/// # Examples
///
/// ```text
/// use io::{wait_ready};
/// let ready = wait_ready(); // warning E0129: `wait_ready` is deprecated
/// ```
fn wait_ready() -> int {}

/// Returns `Ok(())` when `n >= 0` and `Err(IoError::InvalidInput)` otherwise.
///
/// A small host function for testing how `Result<unit, IoError>` values
/// cross between the runtime and Coil code; it does no IO.
///
/// # Examples
///
/// ```
/// use io::{result_unit_probe, IoError};
/// # use tests::assert;
/// let ok = match result_unit_probe(1) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let err = match result_unit_probe(-1) {
///     Result::Err(IoError::InvalidInput) => true,
///     default => false,
/// };
/// assert!(ok && err);
/// ```
fn result_unit_probe(int n) -> Result<unit, IoError> {}

/// Installs package IO hooks on `s` in place; the free-function form of `Stream.attach`.
///
/// `ptr` is the package's session pointer and `read`, `write`, `shutdown`
/// and `free` are C function pointers (as `int`, usually looked up through
/// `dload`). Afterwards `read` / `write` on `s` go through those hooks and
/// `close` calls `shutdown` then `free`. Returns `s`.
///
/// Returns `Err(IoError::InvalidInput)` when `ptr` is `0` or `s` is already
/// attached, and `Err(IoError::AlreadyClosed)` on a closed stream. A program
/// that reaches `attach` compiles only with `--allow-attach`.
///
/// # Examples
///
/// ```text
/// use io::{attach};
/// use io::net::tcp::{connect};
/// let s = connect("example.com", 443)?;
/// // session, rd, wr, sd, fr: a session pointer and C hooks from a dload'ed library
/// let tls = attach(s, session, rd, wr, sd, fr)?;
/// ```
fn attach(Stream s, int ptr, int read, int write, int shutdown, int free) -> Result<Stream, IoError> {}

/// Waits until the descriptor of `s` is ready; the free-function form of `Stream.park`.
///
/// Waits for readability, or for writability when an attached package's
/// last write stopped with `WouldBlock`. Inside a task only that task waits.
///
/// # Examples
///
/// ```
/// use io::{park, read, IoError};
/// use io::net::tcp::{connect, listen, local_addr};
/// use io::sync::{accept_wait, write_all};
/// use string::{to_bytes};
/// # use tests::assert;
/// fn run() -> Result<Option<int>, IoError> {
///     let listener = listen("127.0.0.1", 0)?;
///     let (_, port) = local_addr(listener)?;
///     let client = connect("127.0.0.1", port)?;
///     let server = accept_wait(listener)?;
///     write_all(client, to_bytes("hi"))?;
///     park(server)?;
///     return read(server, to_bytes("....."))?;
/// }
///
/// fn main() {
///     let n = match run() {
///         Result::Ok(Option::Some(n)) => n,
///         default => 0,
///     };
///     assert!(n == 2);
/// }
/// ```
fn park(Stream s) -> Result<unit, IoError> {}
