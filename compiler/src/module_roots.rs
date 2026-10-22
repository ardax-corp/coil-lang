//! `use` / `mod` search roots and module namespaces.
//!
//! Roots are CLI (`--root`) / [`crate::Pipeline`] state.

use std::path::{Path, PathBuf};

/// Default `use`/`mod` search roots (`src` relative to the project directory).
pub fn default_module_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("src")]
}

/// Installed coil-stdlib roots that exist as directories, in search order:
/// `$COIL_STDLIB_DIR`, then its alias `$COIL_STDLIB`, then
/// `~/.coil/stdlib/src` (where spool installs it).
///
/// A variable may name the checkout or its `src`: a directory with a `src`
/// subdirectory contributes that `src`, so the checkout's `tests/` and `docs/`
/// never become importable modules.
pub fn stdlib_module_roots() -> Vec<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = var("HOME").or_else(|| var("USERPROFILE"));
    stdlib_roots_from(var("COIL_STDLIB_DIR"), var("COIL_STDLIB"), home)
}

fn stdlib_roots_from(
    stdlib_dir: Option<PathBuf>,
    stdlib_alias: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for dir in [stdlib_dir, stdlib_alias].into_iter().flatten() {
        let src = dir.join("src");
        candidates.push(if src.is_dir() { src } else { dir });
    }
    if let Some(home) = home {
        candidates.push(home.join(".coil").join("stdlib").join("src"));
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for dir in candidates {
        if dir.is_dir() && !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    roots
}

/// Top-level modules of coil-stdlib (`io::sync`, `collections::…`, `text`).
const STDLIB_MODULES: &[&str] = &[
    "arith",
    "ascii",
    "bytes",
    "cell",
    "collections",
    "conv",
    "encoding",
    "fmt",
    "io",
    "num",
    "path",
    "random",
    "text",
];

/// E0900 help for an unresolved `use` of a coil-stdlib module when no search
/// root holds that module at all (no stdlib root is configured).
pub fn stdlib_root_help(roots: &[PathBuf], project_root: &Path, path: &[String]) -> Option<String> {
    let first = path.first()?;
    if !STDLIB_MODULES.contains(&first.as_str()) {
        return None;
    }
    let present = roots.iter().any(|root| {
        let dir = abs_search_root(project_root, root);
        dir.join(first).is_dir() || dir.join(format!("{first}.hy")).is_file()
    });
    if present {
        return None;
    }
    Some(format!(
        "coil-stdlib provides `{first}::…` modules and no stdlib root was found: pass \
         `--root <coil-stdlib>/src`, set `COIL_STDLIB_DIR` to a coil-stdlib checkout, \
         or install it into `~/.coil/stdlib` with spool"
    ))
}

fn abs_search_root(project_root: &Path, root: &Path) -> PathBuf {
    if root.as_os_str().is_empty() || root == Path::new(".") {
        project_root.to_path_buf()
    } else {
        project_root.join(root)
    }
}

/// Resolve a `use` target (`a::b::c`) to an absolute file
/// path. Searches each search root in order; the first
/// match wins. Returns `None` if no root contains the
/// module file.
///
/// `path` is the segments of the module path BEFORE the
/// item name (e.g. `["a", "b"]` for `use a::b::c;`).
/// `name` is the final segment (e.g. `"c"`).
///
/// Resolution tries, in order:
/// 1. **One-item-per-file:** `<root>/<path>/<name>.hy`
///    (e.g. `use foo::sadge;` → `foo/sadge.hy`)
/// 2. **Item-in-module-file:** `<root>/<path>.hy`
///    (e.g. `use foo::sadge;` → `foo.hy` when the item
///    `sadge` lives inside that module file)
///
/// If both exist, Convention A wins silently (documented in
/// coil-website `src/content/docs/references/modules.md`
/// (`/docs/references/modules`) under Path resolution /
/// Shadowing). Brace/glob imports against a module file are
/// unaffected when only Convention B is present.
///
/// The fully qualified name of the imported item depends
/// on which file was loaded — see codegen's alias map.
pub fn resolve_use_in_roots(
    roots: &[PathBuf],
    project_root: &Path,
    path: &[String],
    name: &str,
) -> Option<PathBuf> {
    for root in roots {
        let mut candidate = abs_search_root(project_root, root);
        for segment in path {
            candidate.push(segment);
        }
        candidate.push(format!("{}.hy", name));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    if let Some(module_stem) = path.last() {
        let dir_segments = &path[..path.len() - 1];
        for root in roots {
            let mut candidate = abs_search_root(project_root, root);
            for segment in dir_segments {
                candidate.push(segment);
            }
            candidate.push(format!("{}.hy", module_stem));
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `mod name;` → `<root>/name.hy` in each search root.
pub fn resolve_mod_in_roots(roots: &[PathBuf], project_root: &Path, name: &str) -> Option<PathBuf> {
    for root in roots {
        let candidate = abs_search_root(project_root, root).join(format!("{}.hy", name));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Compute the namespace of a file given its absolute
/// path and the project root. The namespace is the path
/// of the file relative to the FIRST search root that
/// contains it, with the file extension stripped and
/// path separators replaced with `::`.
///
/// For example, given roots `["./src", "./builtins"]` and
/// file `./builtins/core/ffi/dload.hy`, the namespace is
/// `core::ffi::dload`.
///
/// Returns `None` if the file is not inside any search
/// root. Files outside any search root are still
/// compilable (we use their bare stem as the namespace),
/// but the caller is expected to handle that fallback.
///
/// With nested roots the innermost one that contains `file` wins.
pub fn namespace_of_in_roots(
    roots: &[PathBuf],
    project_root: &Path,
    file: &Path,
) -> Option<String> {
    // Nested roots (`.` and `.deps/pkg/src`) both contain a dependency file;
    // the innermost one gives the namespace `use` resolves it by (`pkg`, not
    // `.deps::pkg::src::pkg`), so take the shortest relative path (#581).
    // Ties keep root order.
    roots
        .iter()
        .filter_map(|root| {
            let abs_root = abs_search_root(project_root, root);
            file.strip_prefix(&abs_root).ok().map(Path::to_path_buf)
        })
        .min_by_key(|rel| rel.components().count())
        .map(|rel| path_to_namespace(&rel))
}

/// Convert a relative file path to a namespace string. Strips
/// the file extension and replaces path separators with `::`.
///
/// `"core/ffi/dload.hy"` → `"core::ffi::dload"`
/// `"foo.hy"` → `"foo"`
fn path_to_namespace(rel: &Path) -> String {
    // Strip the file extension.
    let stem = rel.with_extension("");
    // Convert path separators to `::`.
    let mut ns = String::new();
    let mut first = true;
    for component in stem.components() {
        if let std::path::Component::Normal(s) = component {
            if !first {
                ns.push_str("::");
            }
            ns.push_str(&s.to_string_lossy());
            first = false;
        }
    }
    ns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdlib_roots_prefer_src_and_add_the_spool_install() {
        let tmp =
            std::env::temp_dir().join(format!("coil_module_roots_stdlib_{}", std::process::id()));
        let checkout = tmp.join("checkout");
        let alias = tmp.join("alias_src");
        let home = tmp.join("home");
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        std::fs::create_dir_all(&alias).unwrap();
        std::fs::create_dir_all(home.join(".coil/stdlib/src")).unwrap();

        let roots = stdlib_roots_from(
            Some(checkout.clone()),
            Some(alias.clone()),
            Some(home.clone()),
        );
        assert_eq!(
            roots,
            vec![
                checkout.join("src"),
                alias.clone(),
                home.join(".coil/stdlib/src"),
            ]
        );
        // Missing directories are skipped; one directory is listed once.
        let roots = stdlib_roots_from(Some(tmp.join("missing")), Some(alias.clone()), None);
        assert_eq!(roots, vec![alias.clone()]);
        let roots = stdlib_roots_from(Some(alias.clone()), Some(alias.clone()), None);
        assert_eq!(roots, vec![alias.clone()]);
        assert!(stdlib_roots_from(None, None, Some(tmp.join("nohome"))).is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stdlib_root_help_only_when_no_root_holds_the_module() {
        let tmp = std::env::temp_dir().join(format!(
            "coil_module_roots_stdlib_help_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(tmp.join("lib/io")).unwrap();
        let src = [PathBuf::from("src")];
        let io_sync = ["io".to_string(), "sync".to_string()];
        let help = stdlib_root_help(&src, &tmp, &io_sync).expect("help for io::sync");
        assert!(help.contains("COIL_STDLIB_DIR"), "{help}");
        assert!(stdlib_root_help(&src, &tmp, &["nowhere".to_string()]).is_none());
        let with_stdlib = [PathBuf::from("src"), PathBuf::from("lib")];
        assert!(stdlib_root_help(&with_stdlib, &tmp, &io_sync).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn path_to_namespace_strips_extension_and_uses_double_colon() {
        assert_eq!(path_to_namespace(Path::new("foo.hy")), "foo");
        assert_eq!(
            path_to_namespace(Path::new("core/ffi/dload.hy")),
            "core::ffi::dload"
        );
        assert_eq!(path_to_namespace(Path::new("a/b/c.hy")), "a::b::c");
    }

    #[test]
    fn resolve_use_finds_file_in_first_root() {
        // Build a temporary project layout:
        //   <tmp>/src/foo/sadge.hy
        // `use foo::sadge;` should resolve to that file.
        let tmp = std::env::temp_dir().join("coil_module_roots_test_1");
        let src = tmp.join("src").join("foo");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("sadge.hy"), "// empty\n").unwrap();

        let roots = default_module_roots();
        let resolved = resolve_use_in_roots(&roots, &tmp, &["foo".into()], "sadge");
        assert!(
            resolved.is_some(),
            "expected to find sadge.hy in <tmp>/src/foo/"
        );
        let resolved = resolved.unwrap();
        assert!(resolved.ends_with("src/foo/sadge.hy"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_use_falls_back_to_second_root() {
        let tmp = std::env::temp_dir().join("coil_module_roots_test_2");
        let vendor = tmp.join("vendor").join("lib_x");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("foo.hy"), "// empty\n").unwrap();

        let roots = vec![PathBuf::from("src"), PathBuf::from("vendor")];
        let resolved = resolve_use_in_roots(&roots, &tmp, &["lib_x".into()], "foo");
        assert!(
            resolved.is_some(),
            "expected to find foo.hy in vendor/lib_x/"
        );
        let resolved = resolved.unwrap();
        assert!(resolved.ends_with("vendor/lib_x/foo.hy"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_use_falls_back_to_module_file() {
        // Layout: <tmp>/src/foo.hy (no foo/sadge.hy).
        // `use foo::sadge;` should resolve to foo.hy.
        let tmp = std::env::temp_dir().join("coil_module_roots_test_module_file");
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("foo.hy"), "fn sadge() {}\n").unwrap();

        let roots = default_module_roots();
        let resolved = resolve_use_in_roots(&roots, &tmp, &["foo".into()], "sadge");
        assert!(
            resolved.is_some(),
            "expected to fall back to <tmp>/src/foo.hy"
        );
        assert!(resolved.unwrap().ends_with("src/foo.hy"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_use_prefers_one_item_file_over_module_file() {
        // Both Convention A (`foo/sadge.hy`) and B (`foo.hy`) exist —
        // A must win so FQN/body resolution stays deterministic.
        let tmp = std::env::temp_dir().join("coil_module_roots_test_prefers_a");
        let src = tmp.join("src");
        let sub = src.join("foo");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(src.join("foo.hy"), "fn sadge() { /* module file */ }\n").unwrap();
        std::fs::write(sub.join("sadge.hy"), "fn sadge() { /* one-item */ }\n").unwrap();

        let roots = default_module_roots();
        let resolved = resolve_use_in_roots(&roots, &tmp, &["foo".into()], "sadge");
        assert!(resolved.is_some());
        let path = resolved.unwrap();
        assert!(
            path.ends_with("src/foo/sadge.hy"),
            "expected Convention A path, got {}",
            path.display()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_use_returns_none_when_missing() {
        let tmp = std::env::temp_dir().join("coil_module_roots_test_3");
        std::fs::create_dir_all(&tmp).unwrap();

        let roots = default_module_roots();
        let resolved = resolve_use_in_roots(&roots, &tmp, &["nonexistent".into()], "missing");
        assert!(resolved.is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_mod_finds_top_level_file() {
        let tmp = std::env::temp_dir().join("coil_module_roots_test_resolve_mod");
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("foo.hy"), "// empty\n").unwrap();

        let roots = default_module_roots();
        let resolved = resolve_mod_in_roots(&roots, &tmp, "foo");
        assert!(resolved.is_some());
        assert!(resolved.unwrap().ends_with("src/foo.hy"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn namespace_of_returns_path_relative_to_root() {
        let tmp = std::env::temp_dir().join("coil_module_roots_test_4");
        let builtins = tmp.join("builtins").join("core").join("ffi");
        std::fs::create_dir_all(&builtins).unwrap();
        let file = builtins.join("dload.hy");
        std::fs::write(&file, "// empty\n").unwrap();

        let roots = vec![PathBuf::from("src"), PathBuf::from("builtins")];
        let ns = namespace_of_in_roots(&roots, &tmp, &file);
        assert_eq!(ns, Some("core::ffi::dload".to_string()));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn namespace_of_dot_root_strips_project_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "coil_module_roots_dot_root_{}",
            std::process::id()
        ));
        let nested = tmp.join("a");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("foo.hy");
        std::fs::write(&file, "// empty\n").unwrap();

        let ns = namespace_of_in_roots(&[PathBuf::from(".")], &tmp, &file);
        assert_eq!(ns.as_deref(), Some("a::foo"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn namespace_of_prefers_the_innermost_root() {
        // #581: `.` before `.deps/shapes/src`, as the LSP lists them.
        let tmp = std::env::temp_dir().join(format!(
            "coil_module_roots_nested_roots_{}",
            std::process::id()
        ));
        let dep_src = tmp.join(".deps").join("shapes").join("src");
        std::fs::create_dir_all(&dep_src).unwrap();
        let file = dep_src.join("shapes.hy");
        std::fs::write(&file, "// empty\n").unwrap();

        let roots = [PathBuf::from("."), PathBuf::from(".deps/shapes/src")];
        let ns = namespace_of_in_roots(&roots, &tmp, &file);
        assert_eq!(ns.as_deref(), Some("shapes"));
        // Order does not matter.
        let reversed = [PathBuf::from(".deps/shapes/src"), PathBuf::from(".")];
        let ns = namespace_of_in_roots(&reversed, &tmp, &file);
        assert_eq!(ns.as_deref(), Some("shapes"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn namespace_of_returns_none_for_file_outside_all_roots() {
        let tmp = std::env::temp_dir().join("coil_module_roots_test_5");
        let outside = tmp.join("totally").join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let file = outside.join("x.hy");
        std::fs::write(&file, "// empty\n").unwrap();

        let roots = vec![PathBuf::from("src")];
        let ns = namespace_of_in_roots(&roots, &tmp, &file);
        assert_eq!(ns, None);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
