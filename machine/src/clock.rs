//! Process clocks for HostInvoke (`clock_wall_nanos`, `clock_mono_nanos`,
//! `clock_sleep_ms`). Instant is a Coil `int` snapshot of mono time — no
//! host-side Instant map.

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::Value;

use crate::memory::Heap;

/// UTC/unix wall time as nanoseconds since the Unix epoch.
pub fn wall_nanos() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => nanos_i64(d),
        Err(e) => -nanos_i64(e.duration()),
    }
}

/// Monotonic nanoseconds from a process-local origin (not wall time).
pub fn mono_nanos() -> i64 {
    nanos_i64(mono_origin().elapsed())
}

/// Block the calling thread for `ms` milliseconds. Negative durations are no-ops.
pub fn sleep_ms(ms: i64) {
    if let Ok(ms) = u64::try_from(ms) {
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

fn mono_origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

fn nanos_i64(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

fn host_wall_nanos(_heap: &mut Heap, args: &[Value]) -> Value {
    debug_assert!(args.is_empty());
    Value::from(wall_nanos())
}

fn host_mono_nanos(_heap: &mut Heap, args: &[Value]) -> Value {
    debug_assert!(args.is_empty());
    Value::from(mono_nanos())
}

fn host_sleep_ms(_heap: &mut Heap, args: &[Value]) -> Value {
    let ms = args.first().map(|v| v.as_int()).unwrap_or(0);
    sleep_ms(ms);
    Value::default()
}

/// Pipeline wiring: `(registry_name, arity, host_fn)`.
pub const CLOCK_WIRING: &[(&str, usize, fn(&mut Heap, &[Value]) -> Value)] = &[
    (common::CLOCK_WALL_NANOS_NATIVE, 0, host_wall_nanos),
    (common::CLOCK_MONO_NANOS_NATIVE, 0, host_mono_nanos),
    (common::CLOCK_SLEEP_MS_NATIVE, 1, host_sleep_ms),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_and_mono_move_forward_across_sleep() {
        let w0 = wall_nanos();
        let m0 = mono_nanos();
        sleep_ms(15);
        let w1 = wall_nanos();
        let m1 = mono_nanos();
        assert!(m1 > m0, "mono must advance: {m0} -> {m1}");
        assert!(
            m1 - m0 >= 5_000_000,
            "mono should cover most of a 15ms sleep: delta={}",
            m1 - m0
        );
        assert!(w1 > w0, "wall must advance: {w0} -> {w1}");
        assert!(w0 > 0, "wall nanos should be after the Unix epoch");
    }

    #[test]
    fn sleep_ms_negative_is_noop() {
        let m0 = mono_nanos();
        sleep_ms(-5);
        let m1 = mono_nanos();
        assert!(m1 >= m0);
        assert!(
            m1 - m0 < 50_000_000,
            "negative sleep must not block: delta={}",
            m1 - m0
        );
    }

    #[test]
    fn clock_wiring_names_and_arities() {
        assert_eq!(CLOCK_WIRING.len(), 3);
        let names: Vec<(&str, usize)> = CLOCK_WIRING.iter().map(|&(n, a, _)| (n, a)).collect();
        assert_eq!(
            names,
            [
                (common::CLOCK_WALL_NANOS_NATIVE, 0),
                (common::CLOCK_MONO_NANOS_NATIVE, 0),
                (common::CLOCK_SLEEP_MS_NATIVE, 1),
            ]
        );
    }

    #[test]
    fn host_wrappers_return_int_and_unit() {
        let mut heap = Heap::default();
        let wall = host_wall_nanos(&mut heap, &[]);
        let mono = host_mono_nanos(&mut heap, &[]);
        assert!(wall.as_int() > 0);
        assert!(mono.as_int() >= 0);
        let unit = host_sleep_ms(&mut heap, &[Value::from(0i64)]);
        assert_eq!(unit, Value::default());
    }
}
