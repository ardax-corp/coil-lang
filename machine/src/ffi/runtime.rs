//! Runtime checks for packaged / embedded programs.

use std::path::Path;

use libloading::Library;

use super::gate::DloadGate;
use super::resolve::resolve_library;

/// Sonames to probe when checking for a dynamically linked libffi (optional `system` feature).
fn libffi_probe_names() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &["libffi.dylib", "libffi.8.dylib", "libffi.7.dylib"]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        &["libffi.so.8", "libffi.so.7", "libffi.so.6", "libffi.so"]
    }
    #[cfg(target_os = "windows")]
    {
        &["libffi-8.dll", "ffi.dll", "libffi.dll"]
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        &[] as &[&str]
    }
}

/// Best-effort check that libffi is available when the binary was linked against the system copy.
///
/// Default `libffi` crate builds static libffi into the runner; this probe usually succeeds
/// even when no system `libffi.so` exists (nothing to open). It is still useful when packaging
/// with `libffi` + `system` or for diagnosing broken installs.
pub fn probe_system_libffi() -> Result<(), String> {
    let names = libffi_probe_names();
    for name in names {
        if unsafe { Library::new(*name) }.is_ok() {
            return Ok(());
        }
    }
    Err(format!(
        "libffi shared library not found on this system (probed: {}). \
         Packaged programs use libffi for FFI; install libffi or rebuild the runner with \
         vendored libffi.\n\
         Arch Linux: pacman -S libffi\n\
         Debian/Ubuntu: apt install libffi8 (or libffi-dev for building)\n\
         Fedora: dnf install libffi",
        names.join(", ")
    ))
}

/// Verify that each named library can be resolved (for `coil package --check-native`).
pub fn check_native_libraries(
    names: &[String],
    base_dir: Option<&Path>,
    gate: &DloadGate,
) -> Result<(), String> {
    if names.is_empty() {
        return Ok(());
    }
    let mut errors = Vec::new();
    for name in names {
        match resolve_library(name, base_dir, &[], gate) {
            Ok(_) => {}
            Err(e) => errors.push(format!("  - {name}: {e}")),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "packaging check failed: shared libraries required by this program are missing:\n{}\n\
             hint: install the libraries or place them next to the packaged executable",
            errors.join("\n")
        ))
    }
}

/// Run FFI readiness checks for a packaged app about to start.
pub fn packaged_app_ffi_startup_check(uses_ffi: bool) -> Result<(), String> {
    if !uses_ffi {
        return Ok(());
    }
    // Informational when static libffi is linked; harmless no-op on success.
    let _ = probe_system_libffi();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_system_libffi_does_not_panic() {
        let _ = probe_system_libffi();
    }
}
