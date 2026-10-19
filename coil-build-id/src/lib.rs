//! The id of the sources `coil` and its helper binaries were built from.
//!
//! `coil test`, `coil fmt` and the other helper commands re-exec a
//! `coil-<name>` binary beside `coil`. Cargo builds only the binaries it is
//! asked for, so after `cargo build --bin coil` the helper can be left from
//! older sources: `coil test` then compiles with another compiler than
//! `coil <file>` does (#1598). `coil` passes its id to the helper
//! ([`mark_helper`]) and the helper refuses to run under a different one
//! ([`check_parent`]).

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// This build's id: the package version and a hash of the workspace sources.
pub const BUILD_ID: &str = env!("COIL_BUILD_ID");

/// `coil`'s build id, as `coil` passes it to a helper.
pub const PARENT_ID_ENV: &str = "COIL_PARENT_BUILD_ID";

/// `coil`'s path, as `coil` passes it to a helper.
pub const PARENT_EXE_ENV: &str = "COIL_PARENT_EXE";

/// Testing only: `coil` passes this id in place of its own.
pub const FAKE_ID_ENV: &str = "COIL_TESTING_FAKE_BUILD_ID";

/// Set on `helper` the id and path of `parent`, the running `coil`.
pub fn mark_helper(helper: &mut Command, parent: &Path) {
    let id = std::env::var_os(FAKE_ID_ENV).unwrap_or_else(|| OsString::from(BUILD_ID));
    helper.env(PARENT_ID_ENV, id).env(PARENT_EXE_ENV, parent);
}

/// In helper `name` (`coil-test`), before anything else in `main`: exit
/// with an error when the `coil` that started it was built from other
/// sources. Run directly (not by `coil`), it checks nothing.
pub fn check_parent(name: &str) {
    let Some(parent_id) = std::env::var_os(PARENT_ID_ENV) else {
        return;
    };
    let parent_exe = std::env::var_os(PARENT_EXE_ENV).unwrap_or_else(|| OsString::from("coil"));
    // SAFETY: called first thing in `main`, before the helper starts any
    // thread. The variables are `coil`'s note to this process; programs it
    // runs (`coil test` cases) should not see them.
    unsafe {
        std::env::remove_var(PARENT_ID_ENV);
        std::env::remove_var(PARENT_EXE_ENV);
    }
    if parent_id == BUILD_ID {
        return;
    }
    eprintln!("{}", mismatch(name, Path::new(&parent_exe), &parent_id.to_string_lossy()));
    std::process::exit(1);
}

fn mismatch(name: &str, parent_exe: &Path, parent_id: &str) -> String {
    let own = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| name.to_string());
    let width = name.len() + 1;
    format!(
        "error: `{name}` was built from different sources than `coil`\n\
         \x20 {:<width$} {} (build {parent_id})\n\
         \x20 {:<width$} {own} (build {BUILD_ID})\n\
         \x20 help: rebuild them together: `cargo build --bin coil --bin {name}` (add `--release` for release binaries)",
        "coil:",
        parent_exe.display(),
        format!("{name}:"),
    )
}
