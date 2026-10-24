// Doc stubs for the builtin `io::tty` module (hover docs; never compiled).

/// Returns whether `s` is a terminal (`isatty`; a console on Windows).
///
/// `stdout()` and `stderr()` are not terminals while `io::capture_begin()`
/// keeps their writes. A closed stream is not a terminal.
///
/// # Examples
///
/// ```
/// use io::{stdout};
/// use io::tty::{is_tty};
/// let color = is_tty(stdout());
/// ```
fn is_tty(Stream s) -> bool {}

/// Returns the size of the terminal `s` as `(columns, rows)`.
///
/// For `stdin()`, `stdout()` or `stderr()` that is not a terminal, the other
/// standard streams are asked too, so a program whose output is piped still
/// learns the size of the terminal it runs in. Returns
/// `Err(IoError::NotATerminal)` when none is a terminal.
///
/// # Examples
///
/// ```
/// use io::{stdout};
/// use io::tty::{size};
/// let cols = match size(stdout()) {
///     Result::Ok(sz) => {
///         let (c, _) = sz;
///         c
///     },
///     Result::Err(_) => 80,
/// };
/// ```
fn size(Stream s) -> Result<(int, int), IoError> {}

/// Puts the terminal `s` in raw mode: input is not echoed and arrives byte
/// by byte, without waiting for Enter.
///
/// With `keep_signals`, Ctrl-C and Ctrl-Z still send their signals;
/// without it they arrive as the bytes `3` and `26`. With `keep_output`,
/// `"\n"` still moves to the start of the next line; without it output is
/// sent as is (write `"\r\n"`).
///
/// The terminal's settings are saved the first time and restored by
/// `leave_raw`, and also when the program ends while still raw: normally,
/// through `env::exit`, on an uncaught panic, and on SIGINT / SIGTERM /
/// SIGHUP / SIGQUIT (Ctrl-C or closing the window on Windows). Calling it
/// again changes the flags; `leave_raw` still restores the first settings.
/// Returns `Err(IoError::NotATerminal)` when `s` is not a terminal.
///
/// # Examples
///
/// ```
/// use io::{stdin, IoError};
/// use io::tty::{enter_raw, leave_raw};
/// fn main() -> Result<(), IoError> {
///     match enter_raw(stdin(), true, true) {
///         Result::Ok(_) => {
///             // ... read keys ...
///             leave_raw(stdin())?;
///         },
///         Result::Err(IoError::NotATerminal) => {},
///         Result::Err(e) => raise e,
///     }
///     return;
/// }
/// ```
fn enter_raw(Stream s, bool keep_signals, bool keep_output) -> Result<(), IoError> {}

/// Restores the settings `enter_raw` saved for the terminal `s`. Does
/// nothing when it is not in raw mode (or not a terminal).
///
/// # Examples
///
/// ```
/// use io::{stdin};
/// use io::tty::{is_raw, leave_raw};
/// # use tests::assert;
/// let _ = leave_raw(stdin());
/// assert!(!is_raw(stdin()));
/// ```
fn leave_raw(Stream s) -> Result<(), IoError> {}

/// Returns whether the terminal `s` is in raw mode (`enter_raw`).
///
/// Raw mode belongs to the terminal, not the stream: two streams on the
/// same terminal agree.
///
/// # Examples
///
/// ```
/// use io::{stdin};
/// use io::tty::{is_raw};
/// let raw = is_raw(stdin());
/// ```
fn is_raw(Stream s) -> bool {}

/// Waits up to `timeout_ms` milliseconds for input on `s` (a negative
/// timeout waits for ever), then reads what is there into `buf`.
///
/// Returns `Ok(Some(n))` with the number of bytes read, `Ok(Some(0))` when
/// the time passed or the wait was cut short by a window resize (check
/// `resized`), and `Ok(None)` at the end of the input. The wait is what
/// tells a lone Esc key from the start of an escape sequence. It blocks the
/// thread (inside a `task::scope`, other tasks wait too). Works on any
/// stream; files are always ready.
///
/// # Examples
///
/// ```
/// use io::{stdin, IoError};
/// use io::tty::{read_timeout};
/// use string::{to_bytes};
/// fn main() -> Result<(), IoError> {
///     let buf = to_bytes("        ");
///     match read_timeout(stdin(), buf, 10)? {
///         Option::Some(0) => {}, // nothing yet
///         Option::Some(_) => {}, // bytes in `buf`
///         Option::None => {}, // stdin is closed
///     }
///     return;
/// }
/// ```
fn read_timeout(Stream s, Vec<byte> buf, int timeout_ms) -> Result<Option<int>, IoError> {}

/// Returns `true` once for each change of the terminal window's size since
/// the last call (SIGWINCH; on Windows the console size is compared with
/// the one last seen). The first call only starts watching.
///
/// # Examples
///
/// ```
/// use io::tty::{resized};
/// if resized() {
///     // redraw at the new size
/// }
/// ```
fn resized() -> bool {}

/// Makes the output console `s` interpret ANSI escape sequences. Needed on
/// Windows consoles, where it returns `false` when `s` is not a console;
/// elsewhere terminals already do and it returns `true`.
///
/// # Examples
///
/// ```
/// use io::{stdout};
/// use io::tty::{enable_vt, is_tty};
/// let ansi = is_tty(stdout()) && enable_vt(stdout());
/// ```
fn enable_vt(Stream s) -> bool {}
