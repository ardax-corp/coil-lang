// Doc stubs for the builtin `clock` module (hover docs; never compiled).

/// Returns the current wall-clock time in nanoseconds since the Unix epoch (UTC).
///
/// The wall clock can jump (NTP, manual changes); measure durations with
/// `mono_nanos` instead. For dates, formatting and parsing, use the
/// `coil-time` package.
///
/// # Examples
///
/// ```
/// use clock::{wall_nanos};
/// # use tests::assert;
/// let secs = wall_nanos() / 1000000000;
/// assert!(secs > 1600000000);
/// ```
fn wall_nanos() -> int {}

/// Returns monotonic nanoseconds from a process-local starting point.
///
/// The value never goes backwards and is unrelated to wall time: only the
/// difference between two readings means something.
///
/// # Examples
///
/// ```
/// use clock::{mono_nanos, sleep_ms};
/// # use tests::assert;
/// let start = mono_nanos();
/// sleep_ms(5);
/// let elapsed_ms = (mono_nanos() - start) / 1000000;
/// assert!(elapsed_ms >= 5);
/// ```
fn mono_nanos() -> int {}

/// Pauses for at least `ms` milliseconds.
///
/// Zero or a negative value returns at once. Outside a `task::scope` this
/// blocks the thread; inside one it suspends only the calling task, so
/// other tasks keep running.
///
/// # Examples
///
/// ```
/// use clock::{sleep_ms, mono_nanos};
/// # use tests::assert;
/// let start = mono_nanos();
/// sleep_ms(10);
/// assert!(mono_nanos() - start >= 10000000);
/// ```
fn sleep_ms(int ms) {}
