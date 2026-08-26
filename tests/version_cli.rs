//! Integration tests for `coil --version` / `-V`.

use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn expected_version_line() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

#[test]
fn version_long_flag_prints_crate_version() {
    let out = Command::new(coil_bin())
        .arg("--version")
        .output()
        .expect("spawn coil --version");
    assert!(
        out.status.success(),
        "coil --version failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim_end(), expected_version_line());
}

#[test]
fn version_short_flag_prints_crate_version() {
    let out = Command::new(coil_bin())
        .arg("-V")
        .output()
        .expect("spawn coil -V");
    assert!(
        out.status.success(),
        "coil -V failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim_end(), expected_version_line());
}
