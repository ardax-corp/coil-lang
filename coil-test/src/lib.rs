//! `coil-test` — the Coil test harness behind `coil test`.
//!
//! Discovers `.hy` files under a root, compiles each in memory with harness
//! cases kept, and runs every `test("…")` case on a fresh VM. Files under a
//! `compile_fail/` path segment must be rejected by the compiler instead.

pub mod args;
pub mod order;
pub mod runner;
