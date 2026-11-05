//! Release-profile lower of a stolen-trailing-if-end fixture (COI-407).
//!
//! `cargo test --release -p compiler --lib` cannot compile: `Instruction`
//! only implements `Debug` under `debug_assertions`. This integration test
//! links the library (not the lib-test harness) and still runs under
//! opt-level 3 / no debug_assertions.
