//! `--wall-timeout`: a case that runs past the limit ends the run.
//!
//! A case the VM never returns from (a loop that depends on a broken
//! invariant, a deadlock between the threads it spawned) would hold the
//! runner, and in CI the whole harness cell, until the job's own limit with
//! nothing in the log naming it. The watchdog thread wakes every second,
//! and once a case in flight has run past the limit without finishing it
//! prints the cases that are stuck (file, case, elapsed time, what the case
//! printed so far) and the seed that reruns the same order, then exits with
//! [`WALL_TIMEOUT_EXIT`]. Nothing else can stop a VM that does not come back
//! to a safepoint, so the process ends, not the case.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use machine::reactor::TestProbe;

/// Exit status of a run the watchdog ended (as `timeout(1)` uses it).
pub const WALL_TIMEOUT_EXIT: i32 = 124;

/// How often the watchdog looks at the cases in flight.
const TICK: Duration = Duration::from_secs(1);

/// A case under watch (returned by [`Watchdog::watch`], consumed by [`Watchdog::done`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchId(u64);

struct Watched {
    file: String,
    case: String,
    started: Instant,
    /// A pool case finishes when its worker says so; an inline case
    /// (`None`) only when [`Watchdog::done`] takes it off the list.
    probe: Option<TestProbe>,
    /// What the case printed so far (shown when it is stuck).
    output: Arc<Mutex<Vec<u8>>>,
}

struct Inner {
    limit: Duration,
    cases: Mutex<BTreeMap<u64, Watched>>,
    next: AtomicU64,
    /// `(stop requested)`, with the condvar the thread sleeps on.
    stop: Mutex<bool>,
    wake: Condvar,
    /// `--seed` text that reruns this order, for the message.
    seed: Option<String>,
    /// Report as a `--json` error event instead of text on stderr.
    json: bool,
}

/// The cases in flight and the thread that watches them. [`Self::off`] watches
/// nothing (no `--wall-timeout`).
pub struct Watchdog {
    inner: Option<Arc<Inner>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    /// No limit: every call is a no-op.
    pub fn off() -> Self {
        Watchdog {
            inner: None,
            thread: None,
        }
    }

    /// Start watching with `limit` per case. `seed` is the `--seed` text of
    /// a shuffled run; `json` selects the event report.
    pub fn start(limit: Duration, seed: Option<String>, json: bool) -> Self {
        let inner = Arc::new(Inner {
            limit,
            cases: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            stop: Mutex::new(false),
            wake: Condvar::new(),
            seed,
            json,
        });
        let worker = Arc::clone(&inner);
        let thread = std::thread::Builder::new()
            .name("coil-test-watchdog".into())
            .spawn(move || worker.run())
            .expect("spawn coil-test watchdog thread");
        Watchdog {
            inner: Some(inner),
            thread: Some(thread),
        }
    }

    /// Put a case on the list. A pool case carries its `probe`; an inline
    /// case (run on the calling thread) none.
    pub fn watch(
        &self,
        file: &str,
        case: &str,
        probe: Option<TestProbe>,
        output: Arc<Mutex<Vec<u8>>>,
    ) -> Option<WatchId> {
        let inner = self.inner.as_ref()?;
        let id = inner.next.fetch_add(1, Ordering::Relaxed);
        inner.lock_cases().insert(
            id,
            Watched {
                file: file.to_string(),
                case: case.to_string(),
                started: Instant::now(),
                probe,
                output,
            },
        );
        Some(WatchId(id))
    }

    /// The case was waited on: off the list.
    pub fn done(&self, id: Option<WatchId>) {
        if let (Some(inner), Some(WatchId(id))) = (&self.inner, id) {
            inner.lock_cases().remove(&id);
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            *inner.stop.lock().unwrap_or_else(|e| e.into_inner()) = true;
            inner.wake.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Inner {
    fn lock_cases(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, Watched>> {
        self.cases.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(&self) {
        let mut stop = self.stop.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *stop {
                return;
            }
            stop = self
                .wake
                .wait_timeout(stop, TICK)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            if *stop {
                return;
            }
            let stuck = self.stuck();
            if !stuck.is_empty() {
                self.report(&stuck);
                std::process::exit(WALL_TIMEOUT_EXIT);
            }
        }
    }

    /// `(file, case, elapsed, output)` of every case past the limit and not finished.
    fn stuck(&self) -> Vec<(String, String, Duration, Vec<u8>)> {
        let cases = self.lock_cases();
        cases
            .values()
            .filter(|w| w.started.elapsed() >= self.limit)
            .filter(|w| w.probe.as_ref().is_none_or(|p| !p.finished()))
            .map(|w| {
                let output = w.output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                (w.file.clone(), w.case.clone(), w.started.elapsed(), output)
            })
            .collect()
    }

    fn report(&self, stuck: &[(String, String, Duration, Vec<u8>)]) {
        let mut text = format!(
            "{} ran past --wall-timeout {}s; the run stops (exit {WALL_TIMEOUT_EXIT})",
            if stuck.len() == 1 { "a case" } else { "cases" },
            self.limit.as_secs()
        );
        for (file, case, elapsed, output) in stuck {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("\n    {file}: {case} ({}s)", elapsed.as_secs()),
            );
            let printed = String::from_utf8_lossy(output);
            let printed = printed.trim_end();
            if !printed.is_empty() {
                text.push_str("\n        output so far:");
                for line in printed.lines() {
                    text.push_str("\n        | ");
                    text.push_str(line);
                }
            }
        }
        if let Some(seed) = &self.seed {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("\nrerun in this order with `--seed {seed}`"),
            );
        }
        if self.json {
            crate::events::emit_error(&text);
        } else {
            eprintln!("\nerror: {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_watches_nothing() {
        let w = Watchdog::off();
        assert_eq!(w.watch("f", "c", None, Arc::default()), None);
        w.done(None);
    }

    #[test]
    fn a_done_case_is_off_the_list_and_a_stuck_one_is_found() {
        let limit = Duration::from_millis(20);
        let inner = Arc::new(Inner {
            limit,
            cases: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            stop: Mutex::new(false),
            wake: Condvar::new(),
            seed: Some("0x7".into()),
            json: false,
        });
        let w = Watchdog {
            inner: Some(Arc::clone(&inner)),
            thread: None,
        };
        let quick = w.watch("tests/a.hy", "quick", None, Arc::default());
        let output = Arc::new(Mutex::new(b"step 1\n".to_vec()));
        let slow = w.watch("tests/b.hy", "slow", None, Arc::clone(&output));
        assert!(inner.stuck().is_empty());
        w.done(quick);
        std::thread::sleep(limit * 2);
        let stuck = inner.stuck();
        assert_eq!(stuck.len(), 1);
        assert_eq!((stuck[0].0.as_str(), stuck[0].1.as_str()), ("tests/b.hy", "slow"));
        assert_eq!(stuck[0].3, b"step 1\n");
        w.done(slow);
        assert!(inner.stuck().is_empty());
    }
}
