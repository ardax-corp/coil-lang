//! Shared-library path resolution.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::Library;

use super::signature::FfiError;

/// Strip a known shared-library suffix and optional `lib` prefix, yielding a stem.
///
/// Examples: `libsum.so` → `sum`, `sum.dll` → `sum`, `libc.so.6` → `c`.
pub(crate) fn library_stem(name: &str) -> String {
    let mut stem = name.to_string();

    // `.so.<version>` (e.g. libc.so.6) before plain `.so`.
    if let Some(idx) = stem.find(".so.") {
        stem.truncate(idx);
    } else if let Some(stripped) = stem.strip_suffix(".so") {
        stem = stripped.to_string();
    } else if let Some(stripped) = stem.strip_suffix(".dylib") {
        stem = stripped.to_string();
    } else if let Some(stripped) = stem.strip_suffix(".dll") {
        stem = stripped.to_string();
    }

    if let Some(stripped) = stem.strip_prefix("lib") {
        // Keep bare names like `lib` itself.
        if !stripped.is_empty() {
            stem = stripped.to_string();
        }
    }
    stem
}

/// First-party stems `dload` may open. Fail-closed; not user-extensible.
pub const DLOAD_PRODUCTION_STEMS: &[&str] = &["crypto", "tls", "regex", "time"];

/// Filename stem for the dload gate (`/abs/libfoo.so` → `foo`).
pub fn dload_request_stem(name: &str) -> String {
    let file = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    library_stem(file)
}

/// Return `Err(LibraryDenied)` unless `name`'s stem is production or `extra_stems`.
pub fn check_dload_allowlist(name: &str, extra_stems: &[&str]) -> Result<(), FfiError> {
    let stem = dload_request_stem(name);
    let allowed =
        DLOAD_PRODUCTION_STEMS.iter().any(|&s| s == stem) || extra_stems.iter().any(|s| *s == stem);
    if allowed {
        Ok(())
    } else {
        Err(FfiError::LibraryDenied {
            name: name.to_string(),
            stem,
        })
    }
}

/// Whether `name` (or its stem) refers to the C standard library.
fn is_libc_alias(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "c" | "libc" | "libc.so.6" | "libsystem" | "libsystem.b.dylib" | "ucrtbase" | "msvcrt"
    ) || {
        let stem = library_stem(&lower);
        matches!(
            stem.as_str(),
            "c" | "system" | "system.b" | "ucrtbase" | "msvcrt"
        )
    }
}

/// Platform-specific candidate file names for a library stem (e.g. `sum`).
fn platform_lib_names(stem: &str) -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        vec![
            format!("{}.dll", stem),
            format!("lib{}.dll", stem),
            stem.to_string(),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            stem.to_string(),
            format!("lib{}.dylib", stem),
            format!("{}.dylib", stem),
        ]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![
            stem.to_string(),
            format!("lib{}.so", stem),
            format!("{}.so", stem),
        ]
    }
}

/// Extra candidates when resolving the C library under portable aliases (`c`, `libc`, …).
fn libc_platform_candidates() -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        vec!["ucrtbase.dll".into(), "msvcrt.dll".into(), "c".into()]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            "c".into(),
            "libSystem.B.dylib".into(),
            "libSystem.dylib".into(),
            "/usr/lib/libSystem.B.dylib".into(),
        ]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![
            "c".into(),
            "libc.so.6".into(),
            "libc.so".into(),
            "/lib/x86_64-linux-gnu/libc.so.6".into(),
            "/lib64/libc.so.6".into(),
            "/usr/lib/libc.so.6".into(),
        ]
    }
}

fn push_candidate(out: &mut Vec<PathBuf>, path: PathBuf) {
    if !out.iter().any(|p| p == &path) {
        out.push(path);
    }
}

/// Filename of a shared library for stem `name` on this platform
/// (`libsum.so` / `libsum.dylib` / `sum.dll`).
pub fn platform_shared_lib_filename(stem: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("{stem}.dll")
    }
    #[cfg(target_os = "macos")]
    {
        format!("lib{stem}.dylib")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        format!("lib{stem}.so")
    }
}

fn push_named_candidates(
    out: &mut Vec<PathBuf>,
    names: &[String],
    base_dir: Option<&Path>,
    search_paths: &[PathBuf],
) {
    for name in names {
        // Absolute libc paths (e.g. /usr/lib/libSystem.B.dylib).
        if Path::new(name).is_absolute() {
            push_candidate(out, PathBuf::from(name));
            continue;
        }
        if let Some(base) = base_dir {
            push_candidate(out, base.join(name));
        }
        for root in search_paths {
            push_candidate(out, root.join(name));
        }
        if let Ok(cwd) = std::env::current_dir() {
            push_candidate(out, cwd.join(name));
        }
        push_candidate(out, PathBuf::from(name));
    }
}

/// Build an ordered list of paths to try for `name`.
pub fn library_candidates(
    name: &str,
    base_dir: Option<&Path>,
    search_paths: &[PathBuf],
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let path = Path::new(name);

    if path.is_absolute() {
        push_candidate(&mut candidates, path.to_path_buf());
        return candidates;
    }

    if name.contains('/') || name.contains('\\') {
        if let Some(base) = base_dir {
            push_candidate(&mut candidates, base.join(name));
        }
        for root in search_paths {
            push_candidate(&mut candidates, root.join(name));
        }
        push_candidate(&mut candidates, PathBuf::from(name));
        if let Ok(cwd) = std::env::current_dir() {
            push_candidate(&mut candidates, cwd.join(name));
        }
        return candidates;
    }

    // Portable libc aliases (`c`, `libc`, `libc.so.6`, …).
    if is_libc_alias(name) {
        push_named_candidates(
            &mut candidates,
            &libc_platform_candidates(),
            base_dir,
            search_paths,
        );
    }

    // Bare / suffixed name: normalize to stem, then try platform extensions.
    let stem = library_stem(name);
    let mut names = platform_lib_names(&stem);
    // Also try the original spelling first (exact user path).
    if !names.iter().any(|n| n == name) {
        names.insert(0, name.to_string());
    }
    push_named_candidates(&mut candidates, &names, base_dir, search_paths);

    candidates
}

/// Resolve and load a shared library, trying each candidate in order.
///
/// Uses the production stem allowlist only ([`DLOAD_PRODUCTION_STEMS`]).
pub fn resolve_library(
    name: &str,
    base_dir: Option<&Path>,
    search_paths: &[PathBuf],
) -> Result<Arc<Library>, FfiError> {
    resolve_library_with_extra_stems(name, base_dir, search_paths, &[])
}

/// Like [`resolve_library`], with extra stems unioned onto the production list.
pub fn resolve_library_with_extra_stems(
    name: &str,
    base_dir: Option<&Path>,
    search_paths: &[PathBuf],
    extra_stems: &[&str],
) -> Result<Arc<Library>, FfiError> {
    check_dload_allowlist(name, extra_stems)?;
    let candidates = library_candidates(name, base_dir, search_paths);
    let mut errors = Vec::new();

    for candidate in &candidates {
        match unsafe { Library::new(candidate) } {
            Ok(lib) => return Ok(Arc::new(lib)),
            Err(e) => errors.push(format!("{}: {e}", candidate.display())),
        }
    }

    Err(FfiError::LibraryNotFound {
        name: name.to_string(),
        tried: candidates.iter().map(|p| p.display().to_string()).collect(),
        detail: errors.join("; "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_stem_strips_so_and_lib_prefix() {
        assert_eq!(library_stem("libsum.so"), "sum");
        assert_eq!(library_stem("sum.so"), "sum");
        assert_eq!(library_stem("libc.so.6"), "c");
        assert_eq!(library_stem("sum"), "sum");
    }

    #[test]
    fn library_stem_strips_dylib_and_dll() {
        assert_eq!(library_stem("libsum.dylib"), "sum");
        assert_eq!(library_stem("sum.dll"), "sum");
        assert_eq!(library_stem("libsum.dll"), "sum");
    }

    #[test]
    fn bare_name_generates_lib_prefix_candidates() {
        let base = PathBuf::from("/proj/examples");
        let c = library_candidates("sum", Some(&base), &[]);
        assert!(c.iter().any(|p| p.ends_with("libsum.so")
            || p.ends_with("libsum.dylib")
            || p.ends_with("sum.dll")
            || p.ends_with("libsum.dll")));
        assert!(c.first().map(|p| p.starts_with(&base)).unwrap_or(false));
    }

    #[test]
    fn suffixed_so_name_still_generates_platform_candidates() {
        let c = library_candidates("libsum.so", None, &[]);
        assert!(c.iter().any(|p| p.ends_with("libsum.so")
            || p.ends_with("libsum.dylib")
            || p.ends_with("sum.dll")
            || p.ends_with("libsum.dll")
            || p.file_name().and_then(|s| s.to_str()) == Some("sum")
            || p.file_name().and_then(|s| s.to_str()) == Some("libsum.so")));
    }

    #[test]
    fn libc_alias_includes_platform_libc() {
        let c = library_candidates("c", None, &[]);
        #[cfg(all(unix, not(target_os = "macos")))]
        assert!(c.iter().any(|p| p.to_string_lossy().contains("libc.so")));
        #[cfg(target_os = "macos")]
        assert!(c
            .iter()
            .any(|p| p.to_string_lossy().contains("libSystem") || p.ends_with("c")));
        #[cfg(target_os = "windows")]
        assert!(c
            .iter()
            .any(|p| p.to_string_lossy().contains("ucrtbase")
                || p.to_string_lossy().contains("msvcrt")));
    }

    #[test]
    fn libc_so_6_alias_resolves_like_c() {
        let a = library_candidates("libc.so.6", None, &[]);
        let b = library_candidates("c", None, &[]);
        // Both should include overlapping libc candidates.
        assert!(!a.is_empty());
        assert!(!b.is_empty());
        let a_has_libc = a.iter().any(|p| {
            let s = p.to_string_lossy();
            s.contains("libc") || s.contains("libSystem") || s.contains("ucrtbase") || s == "c"
        });
        assert!(a_has_libc);
    }

    #[test]
    fn relative_path_uses_base_dir_first() {
        let base = PathBuf::from("/proj");
        let c = library_candidates("vendor/libfoo.so", Some(&base), &[]);
        assert_eq!(c[0], base.join("vendor/libfoo.so"));
    }

    #[test]
    fn absolute_path_is_sole_candidate() {
        let abs = if cfg!(windows) {
            PathBuf::from(r"C:\lib\libfoo.dll")
        } else {
            PathBuf::from("/usr/lib/libfoo.so")
        };
        let c = library_candidates(abs.to_str().unwrap(), None, &[]);
        assert_eq!(c, vec![abs]);
    }

    #[test]
    fn production_stems_are_crypto_tls_regex_time() {
        assert_eq!(DLOAD_PRODUCTION_STEMS, &["crypto", "tls", "regex", "time"]);
    }

    fn assert_denied(name: &str) {
        let expected_stem = dload_request_stem(name);
        match check_dload_allowlist(name, &[]) {
            Err(FfiError::LibraryDenied { stem, .. }) => {
                assert_eq!(stem, expected_stem, "denied stem for {name:?}");
            }
            other => panic!("expected LibraryDenied for {name:?}, got {other:?}"),
        }
        match resolve_library(name, None, &[]) {
            Err(FfiError::LibraryDenied { name: n, stem }) => {
                assert_eq!(n, name);
                assert_eq!(stem, expected_stem);
            }
            other => panic!("expected resolve deny for {name:?}, got {other:?}"),
        }
        match super::super::load_library(name) {
            Err(FfiError::LibraryDenied { stem, .. }) => {
                assert_eq!(stem, expected_stem);
            }
            other => panic!("expected load_library deny for {name:?}, got {other:?}"),
        }
    }

    #[test]
    fn dload_libc_aliases_are_denied() {
        for name in ["c", "libc", "libc.so.6", "libsystem", "ucrtbase", "msvcrt"] {
            assert_denied(name);
        }
    }

    #[test]
    fn dload_unknown_stem_is_denied() {
        assert_denied("notalist");
        assert_denied("sum");
        assert_denied("noop");
    }

    #[test]
    fn dload_absolute_non_allowlisted_path_is_denied() {
        let path = if cfg!(windows) {
            r"C:\Windows\System32\kernel32.dll"
        } else {
            "/lib/x86_64-linux-gnu/libc.so.6"
        };
        assert_denied(path);
        assert_eq!(
            dload_request_stem(path),
            library_stem(Path::new(path).file_name().unwrap().to_str().unwrap())
        );
    }

    fn missing_abs_lib(stem: &str) -> String {
        let name = platform_shared_lib_filename(stem);
        if cfg!(windows) {
            format!(r"C:\coil-dload-missing\{name}")
        } else {
            format!("/coil-dload-missing/{name}")
        }
    }

    #[test]
    fn production_stems_pass_the_gate() {
        for stem in DLOAD_PRODUCTION_STEMS {
            check_dload_allowlist(stem, &[])
                .unwrap_or_else(|e| panic!("production stem {stem} must pass the gate, got {e:?}"));
            let prefixed = platform_shared_lib_filename(stem);
            check_dload_allowlist(&prefixed, &[]).expect("platform filename must map to stem");
            // Absolute missing path: gate uses the filename stem, then dlopen
            // fails closed on a non-existent file. Bare `dload("crypto")` would
            // open system libcrypto on macOS and abort (dyld unsafe-load).
            let missing = missing_abs_lib(stem);
            match resolve_library(&missing, None, &[]) {
                Err(FfiError::LibraryNotFound { .. }) => {}
                Err(FfiError::LibraryDenied { .. }) => {
                    panic!("production stem {stem} must not be denied for {missing}")
                }
                other => panic!("expected missing file for {missing}, got {other:?}"),
            }
        }
    }

    #[test]
    fn extra_stems_allow_fixture_libraries_only_when_passed() {
        check_dload_allowlist("sum", &[]).unwrap_err();
        check_dload_allowlist("sum", &["sum"]).expect("extra stem sum");
        check_dload_allowlist("libsum.so", &["sum"]).expect("libsum.so stem is sum");
        let abs = if cfg!(windows) {
            r"C:\tmp\libsum.dll"
        } else {
            "/tmp/libsum.so"
        };
        check_dload_allowlist(abs, &["sum"]).expect("absolute fixture path uses filename stem");
    }

    /// Default list is the four production stems in this crate (test and lib).
    /// Fixture stems stay off it; extra stems are an explicit test hook.
    #[test]
    fn production_allowlist_excludes_ffi_fixtures_and_is_not_cfg_test() {
        let src = include_str!("resolve.rs");
        let decl = concat!(
            "pub const DLOAD_PRODUCTION_STEMS: &[&str] = ",
            "&[\"crypto\", \"tls\", \"regex\", \"time\"];",
        );
        assert!(
            src.contains(decl),
            "production stems must stay crypto/tls/regex/time"
        );
        let const_idx = src
            .find("pub const DLOAD_PRODUCTION_STEMS")
            .expect("DLOAD_PRODUCTION_STEMS");
        let tests_idx = src
            .find("#[cfg(test)]\nmod tests")
            .expect("cfg(test) module");
        assert!(
            const_idx < tests_idx,
            "DLOAD_PRODUCTION_STEMS must not be defined under #[cfg(test)]"
        );
        assert_eq!(DLOAD_PRODUCTION_STEMS, &["crypto", "tls", "regex", "time"]);
        for fixture in ["sum", "c", "libc", "noop"] {
            assert!(
                !DLOAD_PRODUCTION_STEMS.contains(&fixture),
                "{fixture} must stay off the default list"
            );
        }
    }

    /// Allowed stem, missing file: LibraryNotFound after the gate, not LibraryDenied.
    #[test]
    fn missing_allowed_stem_is_not_found_not_denied() {
        let dir = std::env::temp_dir().join("coil-coi-229-no-such-dload-dir");
        let path_buf = dir.join(platform_shared_lib_filename("crypto"));
        let path = path_buf.to_str().expect("utf-8 path");
        assert!(
            !path_buf.exists(),
            "pin path must not exist on disk: {path}"
        );
        check_dload_allowlist(path, &[]).expect("filename stem crypto must pass the gate");
        match resolve_library(path, None, &[]) {
            Err(FfiError::LibraryNotFound { name, .. }) => assert_eq!(name, path),
            other => panic!("expected LibraryNotFound for missing allowed stem, got {other:?}"),
        }
        match super::super::load_library(path) {
            Err(FfiError::LibraryNotFound { .. }) => {}
            other => panic!("load_library must not deny a missing allowed stem, got {other:?}"),
        }
    }
}
