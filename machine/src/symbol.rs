//! `symbol::next_id()` for HostInvoke (`symbol_next_id`): one process-wide
//! atomic counter. `symbol::fn_key()` needs no runtime part: the compiler
//! turns each call into an `int` literal.

use std::sync::atomic::{AtomicI64, Ordering};

use common::Value;

use crate::memory::Heap;
use crate::HostValueFn;

/// The value the next `next_id()` returns. Shared by every thread, task and
/// VM in the process, so no two calls ever see the same id.
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

/// The next id: `1`, `2`, `3`, … in the order the calls happen.
pub fn next_id() -> i64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn host_next_id(_heap: &mut Heap, args: &[Value]) -> Value {
    debug_assert!(args.is_empty());
    Value::from(next_id())
}

/// Pipeline wiring: `(registry_name, arity, host_fn)`.
pub const SYMBOL_WIRING: &[(&str, usize, HostValueFn)] = &[(common::SYMBOL_NEXT_ID_NATIVE, 0, host_next_id)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_is_unique_across_threads() {
        let handles: Vec<_> = (0..4)
            .map(|_| std::thread::spawn(|| (0..1000).map(|_| next_id()).collect::<Vec<_>>()))
            .collect();
        let mut all: Vec<i64> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n);
        assert!(all.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn next_id_counts_up_by_one() {
        let a = next_id();
        let b = next_id();
        assert!(b > a);
    }
}
