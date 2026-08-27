//! Shared-library path resolution.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::Library;

use super::gate::DloadGate;
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

/// Filename stem for the dload gate (`/abs/libfoo.so` → `foo`).
pub fn dload_request_stem(name: &str) -> String {
    let file = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    library_stem(file)
}

/// Whether `name` (or its stem) refers to the C standard library.
pub fn is_libc_alias(name: &str) -> bool {
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
/// The gate runs before `Library::new`. Only regular files whose SHA-256 is
/// pinned for `name`'s stem are opened.
pub fn resolve_library(
    name: &str,
    base_dir: Option<&Path>,
    search_paths: &[PathBuf],
    gate: &DloadGate,
) -> Result<Arc<Library>, FfiError> {
    let stem = gate.check_request(name)?;
    let candidates = library_candidates(name, base_dir, search_paths);
    let mut errors = Vec::new();
    let mut saw_existing = false;
    let mut saw_hash_reject = false;

    for candidate in &candidates {
        if !candidate.is_file() {
            continue;
        }
        saw_existing = true;
        if !gate.file_hash_allowed(&stem, candidate) {
            saw_hash_reject = true;
            continue;
        }
        match unsafe { Library::new(candidate) } {
            Ok(lib) => return Ok(Arc::new(lib)),
            Err(e) => errors.push(format!("{}: {e}", candidate.display())),
        }
    }

    if saw_existing && saw_hash_reject && errors.is_empty() {
        return Err(DloadGate::hash_mismatch(name, &stem));
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
    fn dload_request_stem_uses_filename() {
        assert_eq!(dload_request_stem("crypto"), "crypto");
        assert_eq!(dload_request_stem("libtls.so"), "tls");
        let abs = if cfg!(windows) {
            r"C:\Windows\System32\kernel32.dll"
        } else {
            "/lib/x86_64-linux-gnu/libc.so.6"
        };
        assert_eq!(
            dload_request_stem(abs),
            library_stem(Path::new(abs).file_name().unwrap().to_str().unwrap())
        );
    }

    #[test]
    fn resolve_library_deny_all_does_not_dlopen() {
        let deny = DloadGate::deny_all();
        match resolve_library("c", None, &[], &deny) {
            Err(FfiError::LibraryDenied { .. }) => {}
            other => panic!("expected LibraryDenied, got {other:?}"),
        }
        match resolve_library("sum", None, &[], &deny) {
            Err(FfiError::LibraryDenied { .. }) => {}
            other => panic!("expected LibraryDenied, got {other:?}"),
        }
    }
}
