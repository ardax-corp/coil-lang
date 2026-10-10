//! `use` / `mod` search roots and module namespaces.
//!
//! Roots are CLI (`--root`) / [`crate::Pipeline`] state. coil never reads
//! `coil.toml`; spool turns `[module].roots` into `--root` flags.

use std::path::{Path, PathBuf};

/// Default `use`/`mod` search roots (`src` relative to the project directory).
pub fn default_module_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("src")]
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_1");
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_2");
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_module_file");
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_prefers_a");
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_3");
        std::fs::create_dir_all(&tmp).unwrap();

        let roots = default_module_roots();
        let resolved = resolve_use_in_roots(&roots, &tmp, &["nonexistent".into()], "missing");
        assert!(resolved.is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_mod_finds_top_level_file() {
        let tmp = std::env::temp_dir().join("coil_manifest_test_resolve_mod");
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_4");
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
            "coil_manifest_dot_root_{}",
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
            "coil_manifest_nested_roots_{}",
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_5");
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
