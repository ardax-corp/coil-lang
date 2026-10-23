// Doc stubs for the builtin `thread` module (hover docs; never compiled).

/// A handle to a function running on another OS thread, from `spawn`.
///
/// Pass it to `join` to wait for the result, or to `detach` to let the
/// thread finish on its own. A `Thread` cannot be sent to another thread.
///
/// `T` is what the thread's function returns, so `join` gives back that
/// type. A bare `Thread` in a type infers its `T`; the channel and lock
/// handles (`Sender<T>`, `Receiver<T>`, `Mutex<T>`, `RwLock<T>`) carry
/// their value type the same way.
///
/// # Examples
///
/// ```
/// use thread::{spawn, join, Thread};
/// # use tests::assert;
/// fn double(int n) -> int {
///     return n * 2;
/// }
///
/// fn main() {
///     let t: Thread<int> = match spawn(double, 21) {
///         Result::Ok(t) => t,
///         Result::Err(_) => panic "spawn failed",
///     };
///     let n = match join(t) {
///         Result::Ok(n) => n,
///         Result::Err(_) => 0,
///     };
///     assert!(n == 42);
/// }
/// ```
class Thread<T> {}

/// The sending half of a channel made by `channel`.
///
/// `Sender` handles can be passed to `spawn` (alone or inside tuples, arrays,
/// records and class instances): every copy refers to the same channel.
/// `close` on any copy closes the channel for everyone.
///
/// # Examples
///
/// ```
/// use thread::{channel, send, recv, Sender, Receiver};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// let s: Sender = tx;
/// let r: Receiver = rx;
/// let _ = send(s, "hi");
/// let got = match recv(r) {
///     Result::Ok(v) => v,
///     Result::Err(_) => "",
/// };
/// assert!(got == "hi");
/// ```
class Sender<T> {}

/// The receiving half of a channel made by `channel`.
///
/// Like `Sender`, a `Receiver` can be passed to other threads; each value
/// sent is received exactly once, by whichever `recv` takes it first.
///
/// # Examples
///
/// ```
/// use thread::{channel, send, try_recv, Receiver, ThreadError};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// let r: Receiver = rx;
/// let empty = match try_recv(r) {
///     Result::Err(ThreadError::WouldBlock) => true,
///     default => false,
/// };
/// assert!(empty);
/// let _ = send(tx, 1);
/// assert!(match try_recv(r) { Result::Ok(v) => v == 1, Result::Err(_) => false });
/// ```
class Receiver<T> {}

/// A mutual-exclusion lock that owns a value, made by `mutex`.
///
/// The value is copied into the lock, and `with_lock` hands the callback a
/// copy and stores the new value it returns. The handle itself can be passed
/// to other threads; every copy refers to the same lock.
///
/// # Examples
///
/// ```
/// use thread::{mutex, with_lock, Mutex};
/// # use tests::assert;
/// let m: Mutex = match mutex(0) {
///     Result::Ok(m) => m,
///     Result::Err(_) => panic "mutex failed",
/// };
/// let _ = with_lock(m, fn (int n) => (n + 5, 0));
/// let now = match with_lock(m, fn (int n) => (n, n)) {
///     Result::Ok(n) => n,
///     Result::Err(_) => -1,
/// };
/// assert!(now == 5);
/// ```
class Mutex<T> {}

/// A readers-writer lock that owns a value, made by `rwlock`.
///
/// Many readers (`with_read`, `try_read`) or one writer (`with_write`,
/// `try_write`) at a time. Like `Mutex`, the handle can be passed to other
/// threads.
///
/// # Examples
///
/// ```
/// use thread::{rwlock, with_read, with_write, RwLock};
/// # use tests::assert;
/// let lock: RwLock = match rwlock(1) {
///     Result::Ok(l) => l,
///     Result::Err(_) => panic "rwlock failed",
/// };
/// let _ = with_write(lock, fn (int n) => (n + 1, 0));
/// let seen = match with_read(lock, fn (int n) => n) {
///     Result::Ok(n) => n,
///     Result::Err(_) => -1,
/// };
/// assert!(seen == 2);
/// ```
class RwLock<T> {}

/// The error of every `thread` function.
///
/// It implements `Show`, so `format("%v", e)` gives the variant name.
///
/// # Examples
///
/// ```
/// use thread::{channel, close, send, ThreadError};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// let _ = close(tx);
/// let closed = match send(tx, 1) {
///     Result::Err(ThreadError::Disconnected) => true,
///     default => false,
/// };
/// assert!(closed);
/// ```
enum ThreadError {
    /// The operation would have to wait: `try_recv` on an empty channel, or
    /// `try_lock` / `try_read` / `try_write` on a lock that is held.
    WouldBlock,
    /// The channel is closed: `send` after `close`, or `recv` / `try_recv`
    /// on a closed channel with no values left.
    Disconnected,
    /// The thread cannot be joined: it was already joined or detached, or it
    /// panicked instead of returning a value.
    JoinFailed,
    /// A value cannot cross to another thread: it is or contains a stream, a
    /// `Thread`, a coroutine, a `gc` handle, a partially applied function, or
    /// a cycle.
    NotSendable,
    /// A readers-writer lock was poisoned by a thread that panicked while
    /// holding it.
    Poisoned,
    /// Any other failure: an `unlock` of a mutex this thread does not hold, a
    /// second `lock` while one is held, or a lock callback that does not
    /// return a pair.
    Other,
}

/// Runs `f(arg)` on another OS thread and returns a handle to it.
///
/// The work goes to a pool of worker threads (one per CPU by default, set
/// with the `COIL_MAX_WORKER_THREADS` environment variable). Each thread has
/// its own heap: `arg` and the `use` captures of a closure are deep-copied
/// to it, and the result is copied back by `join`. Sendable values are
/// numbers, strings, tuples, arrays, records, enums and class instances made
/// of them, plus `Sender`, `Receiver`, `Mutex` and `RwLock` handles, which
/// stay shared. Anything else is `Err(ThreadError::NotSendable)`.
///
/// `spawn(f)` with no argument runs a function that takes none. When `main`
/// returns, the program still waits for threads that were neither joined
/// nor detached.
///
/// # Examples
///
/// ```
/// use thread::{spawn, join};
/// # use tests::assert;
/// fn square(int n) -> int {
///     return n * n;
/// }
///
/// fn main() {
///     let t = match spawn(square, 7) {
///         Result::Ok(t) => t,
///         Result::Err(_) => panic "spawn failed",
///     };
///     let n = match join(t) {
///         Result::Ok(n) => n,
///         Result::Err(_) => 0,
///     };
///     assert!(n == 49);
/// }
/// ```
fn spawn<A, B>(A -> B f, A arg) -> Result<Thread<B>, ThreadError> {}

/// Waits for the thread to finish and returns the value its function returned.
///
/// Returns `Err(ThreadError::JoinFailed)` if the thread was already joined
/// or detached, or if its function panicked (the panic message is printed to
/// stderr). Inside a `task::scope`, only the joining task waits; other tasks
/// keep running.
///
/// # Examples
///
/// ```
/// use thread::{spawn, join, ThreadError};
/// # use tests::assert;
/// fn answer() -> int {
///     return 42;
/// }
///
/// fn main() {
///     let t = match spawn(answer) {
///         Result::Ok(t) => t,
///         Result::Err(_) => panic "spawn failed",
///     };
///     assert!(match join(t) { Result::Ok(n) => n == 42, Result::Err(_) => false });
///     let again = match join(t) {
///         Result::Err(ThreadError::JoinFailed) => true,
///         default => false,
///     };
///     assert!(again);
/// }
/// ```
fn join<T>(Thread<T> t) -> Result<T, ThreadError> {}

/// Lets the thread run to completion without being joined.
///
/// Its result is dropped, and the program does not wait for it when `main`
/// returns. A later `join` on the handle is `Err(ThreadError::JoinFailed)`,
/// as is detaching a thread that was already joined.
///
/// # Examples
///
/// ```
/// use thread::{spawn, detach, join, ThreadError};
/// # use tests::assert;
/// fn work() -> int {
///     return 1;
/// }
///
/// fn main() {
///     let t = match spawn(work) {
///         Result::Ok(t) => t,
///         Result::Err(_) => panic "spawn failed",
///     };
///     assert!(match detach(t) { Result::Ok(_) => true, Result::Err(_) => false });
///     let joined = match join(t) {
///         Result::Err(ThreadError::JoinFailed) => true,
///         default => false,
///     };
///     assert!(joined);
/// }
/// ```
fn detach<T>(Thread<T> t) -> Result<unit, ThreadError> {}

/// Creates an unbounded channel and returns its `(Sender, Receiver)` ends.
///
/// Values sent are copied (they must be sendable, see `spawn`) and received
/// in order. The channel has no capacity limit, so `send` never waits.
///
/// # Examples
///
/// ```
/// use thread::{channel, send, recv, spawn, join, Sender};
/// # use tests::assert;
/// fn produce(Sender tx) -> int {
///     let _ = send(tx, 10);
///     let _ = send(tx, 20);
///     return 0;
/// }
///
/// fn main() {
///     let (tx, rx) = match channel() {
///         Result::Ok(pair) => pair,
///         Result::Err(_) => panic "channel failed",
///     };
///     let t = match spawn(produce, tx) {
///         Result::Ok(t) => t,
///         Result::Err(_) => panic "spawn failed",
///     };
///     let a = match recv(rx) { Result::Ok(v) => v, Result::Err(_) => 0 };
///     let b = match recv(rx) { Result::Ok(v) => v, Result::Err(_) => 0 };
///     let _ = join(t);
///     assert!(a + b == 30);
/// }
/// ```
fn channel<T>() -> Result<(Sender<T>, Receiver<T>), ThreadError> {}

/// Sends a copy of `value` on the channel.
///
/// Never waits, since channels are unbounded. Returns
/// `Err(ThreadError::Disconnected)` once the channel is closed and
/// `Err(ThreadError::NotSendable)` for a value that cannot cross threads.
///
/// # Examples
///
/// ```
/// use thread::{channel, send, recv};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// let _ = send(tx, [1, 2, 3]);
/// let v = match recv(rx) {
///     Result::Ok(v) => v,
///     Result::Err(_) => [0, 0, 0],
/// };
/// assert!(v[2] == 3);
/// ```
fn send<T>(Sender<T> tx, T value) -> Result<unit, ThreadError> {}

/// Takes the next value from the channel, waiting until one arrives.
///
/// Returns `Err(ThreadError::Disconnected)` when the channel is closed and
/// every value has been received. Inside a `task::scope`, only the waiting
/// task is suspended.
///
/// # Examples
///
/// ```
/// use thread::{channel, send, recv, close, ThreadError};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// let _ = send(tx, "last");
/// let _ = close(tx);
/// assert!(match recv(rx) { Result::Ok(s) => s == "last", Result::Err(_) => false });
/// let done = match recv(rx) {
///     Result::Err(ThreadError::Disconnected) => true,
///     default => false,
/// };
/// assert!(done);
/// ```
fn recv<T>(Receiver<T> rx) -> Result<T, ThreadError> {}

/// Sends `value` without waiting.
///
/// Channels are unbounded, so this behaves exactly like `send`: it fails
/// only with `Disconnected` (closed channel) or `NotSendable`.
///
/// # Examples
///
/// ```
/// use thread::{channel, try_send, try_recv};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// assert!(match try_send(tx, 7) { Result::Ok(_) => true, Result::Err(_) => false });
/// assert!(match try_recv(rx) { Result::Ok(v) => v == 7, Result::Err(_) => false });
/// ```
fn try_send<T>(Sender<T> tx, T value) -> Result<unit, ThreadError> {}

/// Takes the next value if there is one, without waiting.
///
/// Returns `Err(ThreadError::WouldBlock)` when the channel is empty but
/// open, and `Err(ThreadError::Disconnected)` when it is empty and closed.
///
/// # Examples
///
/// ```
/// use thread::{channel, send, try_recv, ThreadError};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// let empty = match try_recv(rx) {
///     Result::Err(ThreadError::WouldBlock) => true,
///     default => false,
/// };
/// assert!(empty);
/// let _ = send(tx, 3);
/// assert!(match try_recv(rx) { Result::Ok(v) => v == 3, Result::Err(_) => false });
/// ```
fn try_recv<T>(Receiver<T> rx) -> Result<T, ThreadError> {}

/// Closes the channel: no more sends.
///
/// Later `send` / `try_send` calls fail with `Disconnected`. Receivers still
/// get the values already queued, then `Disconnected`. Closing twice is fine.
///
/// # Examples
///
/// ```
/// use thread::{channel, close, send, ThreadError};
/// # use tests::assert;
/// let (tx, rx) = match channel() {
///     Result::Ok(pair) => pair,
///     Result::Err(_) => panic "channel failed",
/// };
/// assert!(match close(tx) { Result::Ok(_) => true, Result::Err(_) => false });
/// let refused = match send(tx, 1) {
///     Result::Err(ThreadError::Disconnected) => true,
///     default => false,
/// };
/// assert!(refused);
/// ```
fn close<T>(Sender<T> tx) -> Result<unit, ThreadError> {}

/// Creates a mutex holding a copy of `initial`.
///
/// The value must be sendable (see `spawn`), or the result is
/// `Err(ThreadError::NotSendable)`. Read and change it with `with_lock`.
///
/// # Examples
///
/// ```
/// use thread::{mutex, with_lock, spawn, join, Mutex};
/// # use tests::assert;
/// fn bump(Mutex m) -> int {
///     let _ = with_lock(m, fn (int n) => (n + 1, 0));
///     return 0;
/// }
///
/// fn main() {
///     let m = match mutex(0) {
///         Result::Ok(m) => m,
///         Result::Err(_) => panic "mutex failed",
///     };
///     let t1 = match spawn(bump, m) { Result::Ok(t) => t, Result::Err(_) => panic "spawn" };
///     let t2 = match spawn(bump, m) { Result::Ok(t) => t, Result::Err(_) => panic "spawn" };
///     let _ = join(t1);
///     let _ = join(t2);
///     let n = match with_lock(m, fn (int x) => (x, x)) {
///         Result::Ok(n) => n,
///         Result::Err(_) => -1,
///     };
///     assert!(n == 2);
/// }
/// ```
fn mutex<T>(T initial) -> Result<Mutex<T>, ThreadError> {}

/// Locks `m`, runs `f` on its value, stores the new value and returns the result.
///
/// `f` gets the current value and returns a pair `(new_value, result)`: the
/// mutex keeps `new_value` and `with_lock` returns `Ok(result)`. The lock is
/// held for the whole callback, so the update is atomic. If the mutex is
/// held, it waits (inside a `task::scope`, only this task waits). This is the
/// safe way to use a `Mutex`; prefer it over `lock` / `unlock`.
///
/// # Examples
///
/// ```
/// use thread::{mutex, with_lock};
/// # use tests::assert;
/// let m = match mutex(10) {
///     Result::Ok(m) => m,
///     Result::Err(_) => panic "mutex failed",
/// };
/// let before = match with_lock(m, fn (int n) => (n * 2, n)) {
///     Result::Ok(old) => old,
///     Result::Err(_) => -1,
/// };
/// assert!(before == 10);
/// assert!(match with_lock(m, fn (int n) => (n, n)) { Result::Ok(n) => n == 20, Result::Err(_) => false });
/// ```
fn with_lock<A, B>(Mutex<A> m, A -> (A, B) f) -> Result<B, ThreadError> {}

/// Locks `m` until `unlock`, waiting while another thread holds it.
///
/// This gives no access to the value (use `with_lock` for that); it only
/// excludes other `lock` / `with_lock` callers. A thread can hold one mutex
/// through `lock` at a time: a second `lock` before `unlock` is
/// `Err(ThreadError::Other)`.
///
/// # Examples
///
/// ```
/// use thread::{mutex, lock, unlock};
/// # use tests::assert;
/// let m = match mutex(0) {
///     Result::Ok(m) => m,
///     Result::Err(_) => panic "mutex failed",
/// };
/// assert!(match lock(m) { Result::Ok(_) => true, Result::Err(_) => false });
/// assert!(match unlock(m) { Result::Ok(_) => true, Result::Err(_) => false });
/// ```
fn lock<T>(Mutex<T> m) -> Result<unit, ThreadError> {}

/// Locks `m` if it is free, without waiting.
///
/// Returns `Err(ThreadError::WouldBlock)` when the mutex is held (also by
/// this thread). On success, release it with `unlock`.
///
/// # Examples
///
/// ```
/// use thread::{mutex, lock, try_lock, unlock, ThreadError};
/// # use tests::assert;
/// let m = match mutex(0) {
///     Result::Ok(m) => m,
///     Result::Err(_) => panic "mutex failed",
/// };
/// let _ = lock(m);
/// let busy = match try_lock(m) {
///     Result::Err(ThreadError::WouldBlock) => true,
///     default => false,
/// };
/// assert!(busy);
/// let _ = unlock(m);
/// assert!(match try_lock(m) { Result::Ok(_) => true, Result::Err(_) => false });
/// let _ = unlock(m);
/// ```
fn try_lock<T>(Mutex<T> m) -> Result<unit, ThreadError> {}

/// Releases a mutex this thread locked with `lock` or `try_lock`.
///
/// Returns `Err(ThreadError::Other)` if this thread does not hold `m`.
///
/// # Examples
///
/// ```
/// use thread::{mutex, lock, unlock, ThreadError};
/// # use tests::assert;
/// let m = match mutex(0) {
///     Result::Ok(m) => m,
///     Result::Err(_) => panic "mutex failed",
/// };
/// let _ = lock(m);
/// assert!(match unlock(m) { Result::Ok(_) => true, Result::Err(_) => false });
/// let not_held = match unlock(m) {
///     Result::Err(ThreadError::Other) => true,
///     default => false,
/// };
/// assert!(not_held);
/// ```
fn unlock<T>(Mutex<T> m) -> Result<unit, ThreadError> {}

/// Creates a readers-writer lock holding a copy of `initial`.
///
/// The value must be sendable (see `spawn`), or the result is
/// `Err(ThreadError::NotSendable)`.
///
/// # Examples
///
/// ```
/// use thread::{rwlock, with_read};
/// # use tests::assert;
/// let lock = match rwlock("config") {
///     Result::Ok(l) => l,
///     Result::Err(_) => panic "rwlock failed",
/// };
/// let n = match with_read(lock, fn (string s) => s.len()) {
///     Result::Ok(n) => n,
///     Result::Err(_) => 0,
/// };
/// assert!(n == 6);
/// ```
fn rwlock<T>(T initial) -> Result<RwLock<T>, ThreadError> {}

/// Runs `f` on a snapshot of the lock's value and returns what `f` returns.
///
/// The read lock is released before `f` runs, so another thread may change
/// the value meanwhile. Waits while a writer holds the lock (blocking every
/// task of the thread, also inside a `task::scope`).
///
/// # Examples
///
/// ```
/// use thread::{rwlock, with_read};
/// # use tests::assert;
/// let lock = match rwlock([1, 2, 3]) {
///     Result::Ok(l) => l,
///     Result::Err(_) => panic "rwlock failed",
/// };
/// let first = match with_read(lock, fn ([int; 3] xs) => xs[0]) {
///     Result::Ok(v) => v,
///     Result::Err(_) => -1,
/// };
/// assert!(first == 1);
/// ```
fn with_read<A, B>(RwLock<A> lock, A -> B f) -> Result<B, ThreadError> {}

/// Takes the write lock, runs `f` and stores the new value it returns.
///
/// `f` returns `(new_value, result)`, as for `with_lock`. The write lock is
/// held through the callback, so no reader or writer sees a half-done
/// update. Waits while the lock is held.
///
/// # Examples
///
/// ```
/// use thread::{rwlock, with_write, with_read};
/// # use tests::assert;
/// let lock = match rwlock(1) {
///     Result::Ok(l) => l,
///     Result::Err(_) => panic "rwlock failed",
/// };
/// let old = match with_write(lock, fn (int n) => (n + 41, n)) {
///     Result::Ok(v) => v,
///     Result::Err(_) => -1,
/// };
/// assert!(old == 1);
/// assert!(match with_read(lock, fn (int n) => n) { Result::Ok(n) => n == 42, Result::Err(_) => false });
/// ```
fn with_write<A, B>(RwLock<A> lock, A -> (A, B) f) -> Result<B, ThreadError> {}

/// Like `with_read`, but returns `Err(ThreadError::WouldBlock)` instead of
/// waiting when a writer holds the lock.
///
/// # Examples
///
/// ```
/// use thread::{rwlock, try_read};
/// # use tests::assert;
/// let lock = match rwlock(5) {
///     Result::Ok(l) => l,
///     Result::Err(_) => panic "rwlock failed",
/// };
/// assert!(match try_read(lock, fn (int n) => n * 2) { Result::Ok(v) => v == 10, Result::Err(_) => false });
/// ```
fn try_read<A, B>(RwLock<A> lock, A -> B f) -> Result<B, ThreadError> {}

/// Like `with_write`, but returns `Err(ThreadError::WouldBlock)` instead of
/// waiting when the lock is held.
///
/// # Examples
///
/// ```
/// use thread::{rwlock, try_write, with_read};
/// # use tests::assert;
/// let lock = match rwlock(1) {
///     Result::Ok(l) => l,
///     Result::Err(_) => panic "rwlock failed",
/// };
/// let _ = try_write(lock, fn (int n) => (n * 10, 0));
/// assert!(match with_read(lock, fn (int n) => n) { Result::Ok(n) => n == 10, Result::Err(_) => false });
/// ```
fn try_write<A, B>(RwLock<A> lock, A -> (A, B) f) -> Result<B, ThreadError> {}
