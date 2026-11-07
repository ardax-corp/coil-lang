//! Machine stacks native code runs on: task and generator fibers' sizes,
//! the running fiber's stack limit, and the deep stack native calls made on
//! a thread's own stack switch to. Shared by the interpreter and the native driver.

/// A task fiber's machine stack: native frames and the interpreter's nested
/// executions under them. Reserved, not committed: pages are touched as
/// they are used. Native frames live here, so it is sized for recursion as
/// deep as the runtime's frame limit allows.
#[cfg(all(not(target_family = "wasm"), target_pointer_width = "64"))]
pub(crate) const FIBER_STACK_BYTES: usize = 256 << 20;
#[cfg(all(not(target_family = "wasm"), not(target_pointer_width = "64")))]
pub(crate) const FIBER_STACK_BYTES: usize = 32 << 20;

/// A generator fiber's machine stack: programs keep many generators live,
/// so theirs stays small.
#[cfg(not(target_family = "wasm"))]
pub(crate) const GEN_STACK_BYTES: usize = 8 << 20;

thread_local! {
    /// The running fiber's stack limit, for native calls on it.
    pub(crate) static FIBER_STACK_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Call right before switching fibers with `owner`, the `&mut` the caller
/// holds. The code on the other side reaches the same object through a raw
/// pointer, which the optimizer cannot see: without this, `&mut`'s `noalias`
/// lets it treat a switch as touching nothing of `owner`, and it deleted the
/// stores around one (a task then suspended through another fiber's yielder).
#[cfg(not(target_family = "wasm"))]
#[inline(always)]
pub(crate) fn fiber_switch_fence<T: ?Sized>(owner: *mut T) {
    std::hint::black_box(owner);
}

/// The stack native calls made on a thread's own stack run on (a thread's
/// few megabytes would end native recursion far sooner than the runtime's frame
/// limit does): the size of a task's.
#[cfg(not(target_family = "wasm"))]
const DEEP_STACK_BYTES: usize = FIBER_STACK_BYTES;

#[cfg(not(target_family = "wasm"))]
thread_local! {
    /// The thread's deep stack while nothing runs on it (`None` while a
    /// native call does, so a nested one stays where it is).
    static DEEP_STACK: std::cell::RefCell<Option<corosensei::stack::DefaultStack>> = const { std::cell::RefCell::new(None) };
    /// Whether this thread's deep stack was made (a failed reservation is
    /// not tried again).
    static DEEP_STACK_MADE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` on the thread's deep stack, or where it is when that stack is in
/// use (a nested call), the call is on a fiber, or no stack could be
/// reserved.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn on_deep_stack<R>(f: impl FnOnce() -> R) -> R {
    if FIBER_STACK_LIMIT.get() != 0 {
        return f();
    }
    let stack = DEEP_STACK.with(|d| d.borrow_mut().take()).or_else(|| {
        if DEEP_STACK_MADE.replace(true) {
            return None;
        }
        corosensei::stack::DefaultStack::new(DEEP_STACK_BYTES).ok()
    });
    let Some(mut stack) = stack else {
        return f();
    };
    use corosensei::stack::Stack as _;
    let limit = stack.limit().get() + 64 * 1024;
    let saved = FIBER_STACK_LIMIT.replace(limit);
    let out = corosensei::on_stack(&mut stack, f);
    FIBER_STACK_LIMIT.set(saved);
    DEEP_STACK.with(|d| *d.borrow_mut() = Some(stack));
    out
}

/// The lowest address a native call may use on the stack it runs on, when
/// that is a fiber's (`None`: the thread's own stack).
pub(crate) fn fiber_stack_limit() -> Option<usize> {
    let l = FIBER_STACK_LIMIT.get();
    (l != 0).then_some(l)
}

/// Panic when recursion runs out of stack.
pub(crate) const STACK_OVERFLOW: &str = "stack overflow: recursion exceeds the stack limit";
