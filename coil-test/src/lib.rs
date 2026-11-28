//! `coil-test` — the Coil test harness behind `coil test`.
//!
//! Discovers `.coil` files under a root, compiles each in memory with harness
//! cases kept, and runs every `test("…")` case on a fresh runtime. Files under a
//! `compile_fail/` path segment must be rejected by the compiler instead.

pub mod args;
pub mod coverage;
pub mod events;
pub mod mutate;
pub mod order;
pub mod runner;
pub mod watchdog;
