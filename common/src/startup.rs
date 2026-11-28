//! What a program does before `main`, in order.

use rkyv::{Archive, Deserialize, Serialize};

/// One step of a program's startup. `entry` names a body.
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug)]
#[rkyv(compare(PartialEq))]
pub enum StartupOp {
    /// Call the body for what it does.
    Call { entry: u32 },
    /// Call the body and store its word in static `slot`.
    Init { entry: u32, slot: u32 },
    /// Restore the snapshot at `at` in the constant pool.
    Restore { at: u32 },
}

/// The body an AOT executable starts in: its startup, then `main`
/// (`coil_native::start_body`). A panic's backtrace leaves its frame out.
pub const NATIVE_START_NAME: &str = "__start";

/// Name prefix of an auto-par loop's chunk worker (`__coil_par_loop_3`):
/// the loop's body over a range, a function of its own.
pub const PAR_LOOP_WORKER_PREFIX: &str = "__coil_par_loop_";

/// Name prefix of an auto-par loop's fork-join function
/// (`__coil_par_loop_fork_3`), which the loop's site calls.
pub const PAR_LOOP_FORK_PREFIX: &str = "__coil_par_loop_fork_";

/// Name prefix of a recursive fork-join's worker (`__coil_par_fib`): it
/// stands in for a call of the function it is named after.
pub const PAR_FORK_PREFIX: &str = "__coil_par_";

/// The function a recursive fork-join worker named `name` stands in for
/// (`__coil_par_fib` is `fib`'s). A backtrace shows the worker's frame as
/// that function's, or not at all.
pub fn par_fork_fn(name: &str) -> Option<&str> {
    if par_loop_fn(name).is_some() {
        return None;
    }
    name.strip_prefix(PAR_FORK_PREFIX).filter(|f| !f.is_empty())
}

/// What a function is to an auto-par loop, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParLoopFn {
    /// The chunk worker: the loop's body.
    Worker,
    /// The fork-join function between the loop's site and its workers.
    Fork,
}

/// Whether `name` is one of the functions the compiler makes for an
/// auto-par loop. A backtrace shows neither: the loop's body runs in the
/// function the loop is written in.
pub fn par_loop_fn(name: &str) -> Option<ParLoopFn> {
    let numbered = |rest: &str| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    if name.strip_prefix(PAR_LOOP_FORK_PREFIX).is_some_and(numbered) {
        return Some(ParLoopFn::Fork);
    }
    name.strip_prefix(PAR_LOOP_WORKER_PREFIX).is_some_and(numbered).then_some(ParLoopFn::Worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn par_loop_fn_names_only_the_loop_functions() {
        assert_eq!(par_loop_fn("__coil_par_loop_1"), Some(ParLoopFn::Worker));
        assert_eq!(par_loop_fn("__coil_par_loop_fork_0"), Some(ParLoopFn::Fork));
        // A fork worker of a function the user named `loop_body`, and the user's own functions.
        for name in ["__coil_par_loop_body", "__coil_par_loop_", "__coil_par_loop_fork_", "__coil_par_fib", "loop_1", "run"] {
            assert_eq!(par_loop_fn(name), None, "{name}");
        }
    }

    #[test]
    fn par_fork_fn_names_the_function_a_worker_stands_in_for() {
        assert_eq!(par_fork_fn("__coil_par_fib"), Some("fib"));
        assert_eq!(par_fork_fn("__coil_par_loop_body"), Some("loop_body"));
        for name in ["__coil_par_loop_1", "__coil_par_loop_fork_0", "__coil_par_", "fib", "par_fib"] {
            assert_eq!(par_fork_fn(name), None, "{name}");
        }
    }
}
