// Doc stubs for the builtin `gc` module (hover docs; never compiled).

/// A strong pin that keeps a value alive, made by `root`.
///
/// While the `Root` itself is reachable, the garbage collector keeps its
/// value alive, even when nothing else refers to it. The usual use is FFI:
/// pin a Coil buffer or callback before handing its address to C code that
/// keeps it. Read the value with `get`, release it with `unroot`. A `Root`
/// cannot be sent to another thread.
///
/// # Examples
///
/// ```
/// use gc::{root, get, Root};
/// # use tests::assert;
/// let pin: Root<Vec<int>> = root(Vec::from([1, 2, 3]));
/// let n = match get(pin) {
///     Option::Some(v) => v.len(),
///     Option::None => 0,
/// };
/// assert!(n == 3);
/// ```
class Root<T> {}

/// A handle that refers to a value without keeping it alive, made by `weak`.
///
/// `upgrade` returns the value while something else still keeps it alive,
/// and `None` once a collection has freed it. Use it for registries and
/// caches that must not extend lifetimes. A `Weak` cannot be sent to
/// another thread.
///
/// # Examples
///
/// ```
/// use gc::{weak, upgrade, Weak};
/// # use tests::assert;
/// let data = Vec::from([1, 2]);
/// let w: Weak<Vec<int>> = weak(data);
/// let alive = match upgrade(w) {
///     Option::Some(v) => v.len() == 2,
///     Option::None => false,
/// };
/// assert!(alive && data.len() == 2);
/// ```
class Weak<T> {}

/// Pins `value` so the garbage collector keeps it alive while the returned
/// `Root` is reachable.
///
/// # Examples
///
/// ```
/// use gc::{root, get};
/// # use tests::assert;
/// let pin = root("kept");
/// assert!(match get(pin) { Option::Some(s) => s == "kept", Option::None => false });
/// ```
fn root<T>(T value) -> Root<T> {}

/// Returns the pinned value, leaving the pin in place.
///
/// Returns `None` after `unroot` has released the pin.
///
/// # Examples
///
/// ```
/// use gc::{root, get, unroot};
/// # use tests::assert;
/// let pin = root(7);
/// assert!(match get(pin) { Option::Some(n) => n == 7, Option::None => false });
/// let _ = unroot(pin);
/// assert!(match get(pin) { Option::Some(_) => false, Option::None => true });
/// ```
fn get<T>(Root<T> pin) -> Option<T> {}

/// Releases the pin and returns the value it held.
///
/// After this the `Root` no longer keeps the value alive, even if the
/// `Root` itself is still reachable. A second `unroot` (or `get`) returns
/// `None`.
///
/// # Examples
///
/// ```
/// use gc::{root, unroot};
/// # use tests::assert;
/// let pin = root("once");
/// assert!(match unroot(pin) { Option::Some(s) => s == "once", Option::None => false });
/// assert!(match unroot(pin) { Option::Some(_) => false, Option::None => true });
/// ```
fn unroot<T>(Root<T> pin) -> Option<T> {}

/// Makes a handle to `value` that does not keep it alive.
///
/// Values that are not heap objects (`int`, `bool`, `float`, ...) are never
/// collected, so their `Weak` always upgrades. `value` can be of any type:
/// a `weak` of an `Option` or a `Result` refers to the heap object it holds.
///
/// # Examples
///
/// ```
/// use gc::{weak, upgrade};
/// # use tests::assert;
/// let w = weak(5);
/// assert!(match upgrade(w) { Option::Some(n) => n == 5, Option::None => false });
/// ```
fn weak<T>(T value) -> Weak<T> {}

/// Returns the value while it is still alive, `None` once it was collected.
///
/// A weak referent is cleared during a collection in which nothing strong
/// (a variable, a field, a `Root`, ...) refers to it any more. Class `drop`
/// finalizers run before weaks are cleared and can still upgrade a weak to
/// the object being dropped.
///
/// # Examples
///
/// ```
/// use gc::{weak, upgrade, collect, Weak};
/// # use tests::assert;
/// fn make() -> Weak<Vec<int>> {
///     let v = Vec::from([1, 2, 3]);
///     // `v`, the only strong reference, ends with this call.
///     return weak(v);
/// }
///
/// fn main() {
///     let w = make();
///     collect();
///     let gone = match upgrade(w) {
///         Option::Some(_) => false,
///         Option::None => true,
///     };
///     assert!(gone);
/// }
/// ```
fn upgrade<T>(Weak<T> handle) -> Option<T> {}

/// Returns the size of the managed heap in bytes.
///
/// This counts only memory the VM's garbage collector manages: not native
/// libraries, stacks or other process memory.
///
/// # Examples
///
/// ```
/// use gc::{heap_bytes};
/// # use tests::assert;
/// let v = Vec::from([1, 2, 3]);
/// assert!(heap_bytes() > 0 && v.len() == 3);
/// ```
fn heap_bytes() -> int {}

/// Runs a full garbage collection now and returns the number of bytes freed.
///
/// Collection normally happens on its own; call this to make finalizers
/// (`fn drop()`) and weak clearing happen at a known point, for example in
/// tests. Values still named by live locals stay alive.
///
/// # Examples
///
/// ```
/// use gc::{collect};
/// # use tests::assert;
/// let freed = collect();
/// assert!(freed >= 0);
/// ```
fn collect() -> int {}
