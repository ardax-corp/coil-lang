//! Project manifest (`coil.toml`) parsing and module path resolution.
//! Format and discovery rules: `docs/references/project-config.md`.

use std::path::{Path, PathBuf};

/// Errors that can occur while loading a `coil.toml` manifest.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // some variants are reserved for future strict-mode validation
pub enum ManifestError {
    /// The manifest file could not be read (I/O error).
    Io(String),
    /// A line failed to parse (invalid syntax).
    Parse { line: usize, message: String },
    /// A required section is missing.
    MissingSection(&'static str),
    /// A required key is missing from a section.
    MissingKey {
        section: &'static str,
        key: &'static str,
    },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(msg) => write!(f, "manifest I/O error: {}", msg),
            ManifestError::Parse { line, message } => {
                write!(f, "manifest parse error at line {}: {}", line, message)
            }
            ManifestError::MissingSection(s) => write!(f, "missing manifest section: [{}]", s),
            ManifestError::MissingKey { section, key } => {
                write!(f, "missing manifest key: {}.{}", section, key)
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// `[package]` metadata. Parsed for spool / tooling; the
/// compiler does not use it for module discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    /// Optional Coil engine semver range (e.g. `">=0.1.0"`). Stored only.
    pub coil: Option<String>,
    /// Optional include-hook path, relative to this package checkout.
    pub include: Option<PathBuf>,
}

/// Current-project lifecycle scripts (`spool install` / `update`).
/// Missing keys are `None` (no-op for later runners).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scripts {
    pub pre_install: Option<PathBuf>,
    pub post_install: Option<PathBuf>,
    pub pre_update: Option<PathBuf>,
    pub post_update: Option<PathBuf>,
}

/// A `[dependencies]` entry: either a git source, or a local path.
///
/// `version` on git deps is optional schema, not a resolved tag; the lock
/// (`rev` + `content_hash`) is the pin. Optional `rev` is stored as parsed
/// schema only — this crate does not resolve or write a lockfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySpec {
    Git {
        url: String,
        version: Option<String>,
        rev: Option<String>,
    },
    Path {
        path: PathBuf,
    },
}

/// Resolved project manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Search roots for module discovery. Each path is
    /// resolved relative to the project root (the directory
    /// containing `coil.toml`).
    pub roots: Vec<PathBuf>,
    /// Optional explicit entry point. When `None`, the
    /// compiler falls back to the file passed on the CLI.
    pub entry: Option<PathBuf>,
    /// Extra directories searched when resolving FFI library paths.
    pub ffi_search_paths: Vec<PathBuf>,
    /// When false, `env::exec` fails at runtime with `ExecDisabled`.
    pub allow_exec: bool,
    /// Optional `[package]` block (`name` + `version`, plus optional `coil` / `include`).
    pub package: Option<PackageInfo>,
    /// `[dependencies]` entries in declaration order.
    pub dependencies: Vec<(String, DependencySpec)>,
    /// `[scripts]` paths for the current project. All keys optional.
    pub scripts: Scripts,
}

impl Default for Manifest {
    /// Default manifest when no `coil.toml` is present:
    /// a single search root at `src/`, no explicit entry
    /// point.
    fn default() -> Self {
        Self {
            roots: vec![PathBuf::from("src")],
            entry: None,
            ffi_search_paths: Vec::new(),
            allow_exec: false,
            package: None,
            dependencies: Vec::new(),
            scripts: Scripts::default(),
        }
    }
}

impl Manifest {
    /// Load a manifest from a project root. If `coil.toml`
    /// exists, parse it. If not, return the default manifest
    /// (just `src/`).
    ///
    /// `project_root` is the directory containing the
    /// `coil.toml` file. Search roots in the manifest are
    /// stored as relative paths; callers should re-root them
    /// when actually searching (see
    /// [`Manifest::resolve_module`]).
    pub fn load(project_root: &Path) -> Result<Self, ManifestError> {
        let manifest_path = project_root.join("coil.toml");
        match std::fs::read_to_string(&manifest_path) {
            Ok(contents) => Self::parse(&contents),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ManifestError::Io(format!(
                "failed to read `{}`: {e}",
                manifest_path.display()
            ))),
        }
    }

    /// Byte range covering the content of `line_num` (1-based), for diagnostics.
    pub fn byte_range_for_line(source: &str, line_num: usize) -> std::ops::Range<usize> {
        if line_num == 0 {
            return 0..0;
        }
        let mut offset = 0usize;
        for (idx, line) in source.lines().enumerate() {
            if idx + 1 == line_num {
                return offset..offset + line.len();
            }
            offset += line.len();
            if offset < source.len() && source.as_bytes()[offset] == b'\n' {
                offset += 1;
            }
        }
        0..0
    }

    /// Parse a manifest from its source text. Exposed for
    /// tests; production code uses [`Self::load`].
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let mut roots: Option<Vec<PathBuf>> = None;
        let mut entry: Option<PathBuf> = None;
        let mut ffi_search_paths: Option<Vec<PathBuf>> = None;
        let mut allow_exec: Option<bool> = None;
        let mut package_name: Option<String> = None;
        let mut package_version: Option<String> = None;
        let mut package_coil: Option<String> = None;
        let mut package_include: Option<PathBuf> = None;
        let mut saw_package_section = false;
        let mut dependencies: Vec<(String, DependencySpec)> = Vec::new();
        let mut scripts = Scripts::default();
        let mut current_section: Option<&'static str> = None;

        for (idx, raw_line) in source.lines().enumerate() {
            let line_num = idx + 1;
            // Strip comment and surrounding whitespace.
            let line = match strip_comment(raw_line) {
                Some(l) => l.trim(),
                None => continue, // line was entirely a comment
            };
            if line.is_empty() {
                continue;
            }

            // Section header: `[name]`.
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                current_section = match name.trim() {
                    "module" => Some("module"),
                    "entry" => Some("entry"),
                    "ffi" => Some("ffi"),
                    "env" => Some("env"),
                    "package" => {
                        saw_package_section = true;
                        Some("package")
                    }
                    "dependencies" => Some("dependencies"),
                    "scripts" => Some("scripts"),
                    other => {
                        return Err(ManifestError::Parse {
                            line: line_num,
                            message: format!("unknown section `[{}]`", other),
                        });
                    }
                };
                continue;
            }

            // Key-value entry: `key = value`.
            let section = current_section.ok_or(ManifestError::Parse {
                line: line_num,
                message: "key-value entry before any section header".to_string(),
            })?;

            let (key, value) = parse_kv(line).ok_or(ManifestError::Parse {
                line: line_num,
                message: format!("expected `key = value`, got `{}`", line),
            })?;

            match (section, key) {
                ("module", "roots") => {
                    let parsed = parse_string_array(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected array of strings, got `{}`", value),
                    })?;
                    roots = Some(parsed.into_iter().map(PathBuf::from).collect());
                }
                ("entry", "file") => {
                    let parsed = parse_string(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected string, got `{}`", value),
                    })?;
                    entry = Some(PathBuf::from(parsed));
                }
                ("ffi", "search_paths") => {
                    let parsed = parse_string_array(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected array of strings, got `{}`", value),
                    })?;
                    ffi_search_paths = Some(parsed.into_iter().map(PathBuf::from).collect());
                }
                ("env", "allow_exec") => {
                    let parsed = parse_bool(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected `true` or `false`, got `{}`", value),
                    })?;
                    allow_exec = Some(parsed);
                }
                ("package", "name") => {
                    let parsed = parse_string(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected string, got `{}`", value),
                    })?;
                    if package_name.is_some() {
                        return Err(ManifestError::Parse {
                            line: line_num,
                            message: "duplicate key `package.name`".to_string(),
                        });
                    }
                    package_name = Some(parsed);
                }
                ("package", "version") => {
                    let parsed = parse_string(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected string, got `{}`", value),
                    })?;
                    if package_version.is_some() {
                        return Err(ManifestError::Parse {
                            line: line_num,
                            message: "duplicate key `package.version`".to_string(),
                        });
                    }
                    package_version = Some(parsed);
                }
                ("package", "coil") => {
                    let parsed = parse_string(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected string, got `{}`", value),
                    })?;
                    if package_coil.is_some() {
                        return Err(ManifestError::Parse {
                            line: line_num,
                            message: "duplicate key `package.coil`".to_string(),
                        });
                    }
                    package_coil = Some(parsed);
                }
                ("package", "include") => {
                    let parsed = parse_string(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!("expected string, got `{}`", value),
                    })?;
                    if package_include.is_some() {
                        return Err(ManifestError::Parse {
                            line: line_num,
                            message: "duplicate key `package.include`".to_string(),
                        });
                    }
                    package_include = Some(PathBuf::from(parsed));
                }
                ("scripts", "pre_install") => {
                    assign_script_path(
                        &mut scripts.pre_install,
                        "scripts.pre_install",
                        value,
                        line_num,
                    )?;
                }
                ("scripts", "post_install") => {
                    assign_script_path(
                        &mut scripts.post_install,
                        "scripts.post_install",
                        value,
                        line_num,
                    )?;
                }
                ("scripts", "pre_update") => {
                    assign_script_path(
                        &mut scripts.pre_update,
                        "scripts.pre_update",
                        value,
                        line_num,
                    )?;
                }
                ("scripts", "post_update") => {
                    assign_script_path(
                        &mut scripts.post_update,
                        "scripts.post_update",
                        value,
                        line_num,
                    )?;
                }
                ("dependencies", dep_name) => {
                    if dependencies.iter().any(|(n, _)| n == dep_name) {
                        return Err(ManifestError::Parse {
                            line: line_num,
                            message: format!("duplicate dependency `{dep_name}`"),
                        });
                    }
                    let spec = parse_dependency_spec(value).ok_or(ManifestError::Parse {
                        line: line_num,
                        message: format!(
                            "expected dependency table `{{ git = \"…\" }}` \
                             or `{{ path = \"…\" }}`, got `{value}`"
                        ),
                    })?;
                    dependencies.push((dep_name.to_string(), spec));
                }
                (section, key) => {
                    return Err(ManifestError::Parse {
                        line: line_num,
                        message: format!("unknown key `{}.{}`", section, key),
                    });
                }
            }
        }

        let package = match (saw_package_section, package_name, package_version) {
            (false, None, None) => None,
            (true, None, None) => {
                return Err(ManifestError::MissingKey {
                    section: "package",
                    key: "name",
                });
            }
            (_, Some(name), Some(version)) => Some(PackageInfo {
                name,
                version,
                coil: package_coil,
                include: package_include,
            }),
            (_, None, Some(_)) => {
                return Err(ManifestError::MissingKey {
                    section: "package",
                    key: "name",
                });
            }
            (_, Some(_), None) => {
                return Err(ManifestError::MissingKey {
                    section: "package",
                    key: "version",
                });
            }
        };

        Ok(Self {
            roots: roots.unwrap_or_else(|| vec![PathBuf::from("src")]),
            entry,
            ffi_search_paths: ffi_search_paths.unwrap_or_default(),
            allow_exec: allow_exec.unwrap_or(false),
            package,
            dependencies,
            scripts,
        })
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
    /// `docs/references/modules.md` under Path resolution /
    /// Shadowing). Brace/glob imports against a module file are
    /// unaffected when only Convention B is present.
    ///
    /// The fully qualified name of the imported item depends
    /// on which file was loaded — see codegen's alias map.
    pub fn resolve_use(&self, project_root: &Path, path: &[String], name: &str) -> Option<PathBuf> {
        // Convention A — one item per file (preferred when both A and B exist):
        //   `use foo::sadge;` → `<root>/foo/sadge.hy`
        //   `use lib::io::read;` → `<root>/lib/io/read.hy`
        for root in &self.roots {
            let mut candidate = project_root.join(root);
            for segment in path {
                candidate.push(segment);
            }
            candidate.push(format!("{}.hy", name));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        // Convention B — item inside a module file (same file as
        // `use path::*;`). Without this fallback, modules that live
        // under a search root as `foo.hy` are only reachable via glob,
        // never via `use foo::item;` / `use foo::{item, …};`.
        //   `use foo::sadge;` → `<root>/foo.hy` (when foo/sadge.hy is absent)
        //   `use lib::io::read;` → `<root>/lib/io.hy`
        if let Some(module_stem) = path.last() {
            let dir_segments = &path[..path.len() - 1];
            for root in &self.roots {
                let mut candidate = project_root.join(root);
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

    /// Resolve a `mod foo;` forward declaration to an
    /// absolute file path. Looks for `<root>/foo.hy` in
    /// each search root.
    pub fn resolve_mod(&self, project_root: &Path, name: &str) -> Option<PathBuf> {
        for root in &self.roots {
            let candidate = project_root.join(root).join(format!("{}.hy", name));
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
    pub fn namespace_of(&self, project_root: &Path, file: &Path) -> Option<String> {
        for root in &self.roots {
            let abs_root = project_root.join(root);
            if let Ok(rel) = file.strip_prefix(&abs_root) {
                return Some(path_to_namespace(rel));
            }
        }
        None
    }
}

/// Strip an inline comment (everything after `#`, but not
/// inside a string). Returns `None` if the line is entirely a
/// comment (or empty after stripping).
fn strip_comment(line: &str) -> Option<&str> {
    // We don't track string boundaries here because our
    // manifest format doesn't allow `#` inside strings in
    // practice (paths and section names don't include `#`).
    // If we ever allow richer values, this becomes more
    // involved.
    match line.find('#') {
        Some(idx) => {
            let stripped = &line[..idx];
            if stripped.trim().is_empty() {
                None
            } else {
                Some(stripped)
            }
        }
        None => Some(line),
    }
}

/// Parse a `key = value` line. Returns `(key, value)` where
/// `value` is the un-parsed RHS (caller decides whether it's
/// a string, array, etc.).
fn parse_kv(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once('=')?;
    Some((key.trim(), rest.trim()))
}

/// Parse a TOML-like double-quoted string. Returns the inner
/// text (without the surrounding quotes). Returns `None` if
/// the value isn't a double-quoted string.
fn parse_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let inner = trimmed.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

fn assign_script_path(
    slot: &mut Option<PathBuf>,
    fq: &str,
    value: &str,
    line_num: usize,
) -> Result<(), ManifestError> {
    let parsed = parse_string(value).ok_or(ManifestError::Parse {
        line: line_num,
        message: format!("expected string, got `{value}`"),
    })?;
    if slot.is_some() {
        return Err(ManifestError::Parse {
            line: line_num,
            message: format!("duplicate key `{fq}`"),
        });
    }
    *slot = Some(PathBuf::from(parsed));
    Ok(())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Parse a TOML-like array of double-quoted strings:
/// `["a", "b", "c"]`. Returns the inner strings in order.
/// Returns `None` if the value isn't a valid array of strings.
fn parse_string_array(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    for piece in inner.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        out.push(parse_string(piece)?);
    }
    Some(out)
}

/// Parse a TOML-like inline table of string values:
/// `{ git = "…" }` or `{ git = "…", version = "^0.2", rev = "abc" }`.
fn parse_inline_table(value: &str) -> Option<Vec<(String, String)>> {
    let trimmed = value.trim();
    let inner = trimmed.strip_prefix('{')?.strip_suffix('}')?;
    let mut out = Vec::new();
    for piece in inner.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let (key, rhs) = parse_kv(piece)?;
        out.push((key.to_string(), parse_string(rhs)?));
    }
    Some(out)
}

/// Parse a `[dependencies]` RHS into [`DependencySpec`].
///
/// Accepted forms:
/// - `{ git = "url" }`
/// - `{ git = "url", version = "^0.2" }`
/// - `{ git = "url", rev = "abc" }`
/// - `{ git = "url", version = "^0.2", rev = "abc" }`
/// - `{ path = "../local" }`
fn parse_dependency_spec(value: &str) -> Option<DependencySpec> {
    let entries = parse_inline_table(value)?;
    let mut git: Option<String> = None;
    let mut version: Option<String> = None;
    let mut path: Option<String> = None;
    let mut rev: Option<String> = None;
    for (key, val) in entries {
        match key.as_str() {
            "git" if git.is_none() => git = Some(val),
            "version" if version.is_none() => version = Some(val),
            "path" if path.is_none() => path = Some(val),
            "rev" if rev.is_none() => rev = Some(val),
            _ => return None,
        }
    }
    match (git, version, path, rev) {
        (Some(url), version, None, rev) => Some(DependencySpec::Git { url, version, rev }),
        (None, None, Some(path), None) => Some(DependencySpec::Path {
            path: PathBuf::from(path),
        }),
        _ => None,
    }
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
    fn default_manifest_has_src_root() {
        let m = Manifest::default();
        assert_eq!(m.roots, vec![PathBuf::from("src")]);
        assert_eq!(m.entry, None);
        assert!(m.package.is_none());
        assert!(m.dependencies.is_empty());
        assert_eq!(m.scripts, Scripts::default());
    }

    #[test]
    fn parse_minimal_manifest() {
        let src = "[module]\nroots = [\"./src\"]\n";
        let m = Manifest::parse(src).unwrap();
        assert_eq!(m.roots, vec![PathBuf::from("./src")]);
        assert_eq!(m.entry, None);
    }

    #[test]
    fn parse_full_manifest() {
        let src = r#"
            # coil project manifest
            [module]
            roots = ["./src", "./vendor", "./builtins"]

            [entry]
            file = "./src/main.hy"
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.roots,
            vec![
                PathBuf::from("./src"),
                PathBuf::from("./vendor"),
                PathBuf::from("./builtins"),
            ]
        );
        assert_eq!(m.entry, Some(PathBuf::from("./src/main.hy")));
    }

    #[test]
    fn parse_comments_and_blank_lines() {
        let src = "# only a comment\n\n# another\n[module]\nroots = [\"./src\"] # trailing\n";
        let m = Manifest::parse(src).unwrap();
        assert_eq!(m.roots, vec![PathBuf::from("./src")]);
    }

    #[test]
    fn parse_missing_module_section_uses_default() {
        // No `[module]` section: fall back to default roots.
        let src = "[entry]\nfile = \"main.hy\"\n";
        let m = Manifest::parse(src).unwrap();
        assert_eq!(m.roots, vec![PathBuf::from("src")]);
        assert_eq!(m.entry, Some(PathBuf::from("main.hy")));
    }

    #[test]
    fn parse_invalid_kv_returns_error() {
        let src = "[module]\nthis is not a kv line\n";
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { line, .. } => assert_eq!(line, 2),
            _ => panic!("expected Parse error, got {:?}", err),
        }
    }

    #[test]
    fn parse_unknown_section_returns_error() {
        let src = "[unknown]\nfoo = \"bar\"\n";
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { line, message } => {
                assert_eq!(line, 1);
                assert!(message.contains("unknown section"));
            }
            _ => panic!("expected Parse error, got {:?}", err),
        }
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
        let tmp = std::env::temp_dir().join("coil_manifest_test_1");
        let src = tmp.join("src").join("foo");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("sadge.hy"), "// empty\n").unwrap();

        let m = Manifest::default(); // roots = ["src"]
        let resolved = m.resolve_use(&tmp, &["foo".into()], "sadge");
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

        let m = Manifest {
            roots: vec![PathBuf::from("src"), PathBuf::from("vendor")],
            entry: None,
            ffi_search_paths: Vec::new(),
            allow_exec: true,
            package: None,
            dependencies: Vec::new(),
            scripts: Scripts::default(),
        };
        let resolved = m.resolve_use(&tmp, &["lib_x".into()], "foo");
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

        let m = Manifest::default();
        let resolved = m.resolve_use(&tmp, &["foo".into()], "sadge");
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

        let m = Manifest::default();
        let resolved = m.resolve_use(&tmp, &["foo".into()], "sadge");
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

        let m = Manifest::default();
        let resolved = m.resolve_use(&tmp, &["nonexistent".into()], "missing");
        assert!(resolved.is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_mod_finds_top_level_file() {
        let tmp = std::env::temp_dir().join("coil_manifest_test_resolve_mod");
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("foo.hy"), "// empty\n").unwrap();

        let m = Manifest::default();
        let resolved = m.resolve_mod(&tmp, "foo");
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

        let m = Manifest {
            roots: vec![PathBuf::from("src"), PathBuf::from("builtins")],
            entry: None,
            ffi_search_paths: Vec::new(),
            allow_exec: true,
            package: None,
            dependencies: Vec::new(),
            scripts: Scripts::default(),
        };
        let ns = m.namespace_of(&tmp, &file);
        assert_eq!(ns, Some("core::ffi::dload".to_string()));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn namespace_of_returns_none_for_file_outside_all_roots() {
        let tmp = std::env::temp_dir().join("coil_manifest_test_5");
        let outside = tmp.join("totally").join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let file = outside.join("x.hy");
        std::fs::write(&file, "// empty\n").unwrap();

        let m = Manifest {
            roots: vec![PathBuf::from("src")],
            entry: None,
            ffi_search_paths: Vec::new(),
            allow_exec: true,
            package: None,
            dependencies: Vec::new(),
            scripts: Scripts::default(),
        };
        let ns = m.namespace_of(&tmp, &file);
        assert_eq!(ns, None);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_falls_back_to_default_when_coil_toml_absent() {
        let tmp = std::env::temp_dir().join("coil_manifest_test_6");
        std::fs::create_dir_all(&tmp).unwrap();
        // Don't create coil.toml.
        let m = Manifest::load(&tmp).unwrap();
        assert_eq!(m.roots, vec![PathBuf::from("src")]);
        assert_eq!(m.entry, None);
        assert!(!m.allow_exec);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_reads_env_allow_exec_true() {
        let tmp = std::env::temp_dir().join("coil_manifest_test_env_on");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("coil.toml"),
            "[env]\nallow_exec = true\n[module]\nroots = [\"./src\"]\n",
        )
        .unwrap();
        let m = Manifest::load(&tmp).unwrap();
        assert!(m.allow_exec);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_reads_env_allow_exec_false() {
        let tmp = std::env::temp_dir().join("coil_manifest_test_env");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("coil.toml"),
            "[env]\nallow_exec = false\n[module]\nroots = [\"./src\"]\n",
        )
        .unwrap();
        let m = Manifest::load(&tmp).unwrap();
        assert!(!m.allow_exec);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_reads_existing_coil_toml() {
        let tmp = std::env::temp_dir().join("coil_manifest_test_7");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("coil.toml"), "[module]\nroots = [\"./vendor\"]\n").unwrap();

        let m = Manifest::load(&tmp).unwrap();
        assert_eq!(m.roots, vec![PathBuf::from("./vendor")]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_package_and_dependencies() {
        let src = r#"
            [package]
            name = "my_app"
            version = "0.1.0"

            [module]
            roots = ["./src", "./.spool/deps"]

            [dependencies]
            http = { git = "https://github.com/coil-lang/http.git", version = "^0.2" }
            local_http = { path = "../local-http" }
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.package,
            Some(PackageInfo {
                name: "my_app".into(),
                version: "0.1.0".into(),
                coil: None,
                include: None,
            })
        );
        assert_eq!(
            m.dependencies,
            vec![
                (
                    "http".into(),
                    DependencySpec::Git {
                        url: "https://github.com/coil-lang/http.git".into(),
                        version: Some("^0.2".into()),
                        rev: None,
                    }
                ),
                (
                    "local_http".into(),
                    DependencySpec::Path {
                        path: PathBuf::from("../local-http"),
                    }
                ),
            ]
        );
        assert_eq!(
            m.roots,
            vec![PathBuf::from("./src"), PathBuf::from("./.spool/deps")]
        );
    }

    #[test]
    fn parse_legacy_manifest_without_package_still_works() {
        let src = "[module]\nroots = [\"./src\"]\n[entry]\nfile = \"./src/main.hy\"\n";
        let m = Manifest::parse(src).unwrap();
        assert!(m.package.is_none());
        assert!(m.dependencies.is_empty());
        assert_eq!(m.scripts, Scripts::default());
        assert_eq!(m.roots, vec![PathBuf::from("./src")]);
        assert_eq!(m.entry, Some(PathBuf::from("./src/main.hy")));
    }

    #[test]
    fn parse_package_requires_name_and_version() {
        let missing_version = "[package]\nname = \"only_name\"\n";
        match Manifest::parse(missing_version).unwrap_err() {
            ManifestError::MissingKey { section, key } => {
                assert_eq!(section, "package");
                assert_eq!(key, "version");
            }
            other => panic!("expected MissingKey, got {other:?}"),
        }

        let missing_name = "[package]\nversion = \"0.1.0\"\n";
        match Manifest::parse(missing_name).unwrap_err() {
            ManifestError::MissingKey { section, key } => {
                assert_eq!(section, "package");
                assert_eq!(key, "name");
            }
            other => panic!("expected MissingKey, got {other:?}"),
        }

        let empty_package = "[package]\n";
        match Manifest::parse(empty_package).unwrap_err() {
            ManifestError::MissingKey { section, key } => {
                assert_eq!(section, "package");
                assert_eq!(key, "name");
            }
            other => panic!("expected MissingKey, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_package_key_errors() {
        let src = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nauthors = \"nope\"\n";
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("unknown key `package.authors`"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_module_preludes_key_errors() {
        // COI-72: dropped advertised key — must stay a hard unknown-key error.
        let src = "[module]\nroots = [\"./src\"]\npreludes = [\"./stdlib/src\"]\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::Parse { line, message } => {
                assert_eq!(line, 3);
                assert!(
                    message.contains("unknown key `module.preludes`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_module_strict_key_errors() {
        // COI-72: dropped advertised key — must stay a hard unknown-key error.
        let src = "[module]\nroots = [\"./src\"]\nstrict = true\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::Parse { line, message } => {
                assert_eq!(line, 3);
                assert!(
                    message.contains("unknown key `module.strict`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_module_prelude_singular_key_errors() {
        // Singular spelling must not become a backdoor for dropped `preludes`.
        let src = "[module]\nroots = [\"./src\"]\nprelude = [\"./stdlib/src\"]\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::Parse { line, message } => {
                assert_eq!(line, 3);
                assert!(
                    message.contains("unknown key `module.prelude`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn coil_toml_example_parses_without_live_dropped_module_keys() {
        // Keep the shipped example aligned with COI-72: it must parse, and
        // dropped `preludes` / `strict` must not reappear as live assignments.
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../coil.toml.example"));
        for (i, raw) in src.lines().enumerate() {
            let code = raw.split('#').next().unwrap_or("").trim();
            if code.is_empty() {
                continue;
            }
            let key = code.split('=').next().unwrap_or("").trim();
            assert!(
                key != "preludes" && key != "strict",
                "line {}: dropped module key must not be live TOML: {raw}",
                i + 1
            );
        }
        let m = Manifest::parse(src).expect("coil.toml.example must parse");
        assert_eq!(
            m.roots,
            vec![
                PathBuf::from("./src"),
                PathBuf::from("./vendor"),
                PathBuf::from("../coil-stdlib/src"),
            ]
        );
        assert!(m.entry.is_none());
    }

    #[test]
    fn parse_dependency_git_without_version() {
        let src = r#"
            [dependencies]
            http = { git = "https://example.com/http.git" }
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.dependencies,
            vec![(
                "http".into(),
                DependencySpec::Git {
                    url: "https://example.com/http.git".into(),
                    version: None,
                    rev: None,
                }
            )]
        );
    }

    #[test]
    fn parse_dependency_git_accepts_rev() {
        let src = r#"
            [dependencies]
            http = { git = "https://example.com/http.git", rev = "abc123" }
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.dependencies,
            vec![(
                "http".into(),
                DependencySpec::Git {
                    url: "https://example.com/http.git".into(),
                    version: None,
                    rev: Some("abc123".into()),
                }
            )]
        );
    }

    #[test]
    fn parse_dependency_git_accepts_version_and_rev() {
        let src = r#"
            [dependencies]
            http = { git = "https://example.com/http.git", version = "^0.2", rev = "abc123" }
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.dependencies,
            vec![(
                "http".into(),
                DependencySpec::Git {
                    url: "https://example.com/http.git".into(),
                    version: Some("^0.2".into()),
                    rev: Some("abc123".into()),
                }
            )]
        );
    }

    #[test]
    fn parse_dependency_rejects_git_and_path_together() {
        let src = r#"
            [dependencies]
            http = { git = "https://example.com/http.git", version = "^1", path = "../x" }
        "#;
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected dependency table"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_dependency_rejects_unknown_inline_key() {
        let src = r#"
            [dependencies]
            http = { git = "https://example.com/http.git", version = "^1", branch = "main" }
        "#;
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected dependency table"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_dependency_rejects_version_without_git() {
        let src = r#"
            [dependencies]
            http = { version = "^1" }
        "#;
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected dependency table"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_dependency_rejects_path_with_version() {
        let src = r#"
            [dependencies]
            http = { path = "../x", version = "^1" }
        "#;
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected dependency table"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_dependency_rejects_path_with_rev() {
        let src = r#"
            [dependencies]
            http = { path = "../x", rev = "abc123" }
        "#;
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected dependency table"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_dependency_rejects_empty_inline_table_and_bare_string() {
        let empty = "[dependencies]\nhttp = { }\n";
        match Manifest::parse(empty).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected dependency table"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }

        let bare = "[dependencies]\nhttp = \"^1.0\"\n";
        match Manifest::parse(bare).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected dependency table"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_dependency_git_accepts_version_before_git() {
        let src = r#"
            [dependencies]
            http = { version = "^0.2", git = "https://example.com/http.git" }
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.dependencies,
            vec![(
                "http".into(),
                DependencySpec::Git {
                    url: "https://example.com/http.git".into(),
                    version: Some("^0.2".into()),
                    rev: None,
                }
            )]
        );
    }

    #[test]
    fn parse_duplicate_package_keys_errors() {
        let dup_name = "[package]\nname = \"a\"\nname = \"b\"\nversion = \"0.1.0\"\n";
        match Manifest::parse(dup_name).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("duplicate key `package.name`"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }

        let dup_version = "[package]\nname = \"a\"\nversion = \"0.1.0\"\nversion = \"0.2.0\"\n";
        match Manifest::parse(dup_version).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("duplicate key `package.version`"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_package_rejects_non_string_values() {
        let bad_name = "[package]\nname = 1\nversion = \"0.1.0\"\n";
        match Manifest::parse(bad_name).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected string"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }

        let bad_version = "[package]\nname = \"a\"\nversion = true\n";
        match Manifest::parse(bad_version).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("expected string"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_duplicate_dependency_errors() {
        let src = r#"
            [dependencies]
            http = { path = "../a" }
            http = { path = "../b" }
        "#;
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("duplicate dependency `http`"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_section_still_errors() {
        let src = "[registry]\nurl = \"https://example.com\"\n";
        let err = Manifest::parse(src).unwrap_err();
        match err {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("unknown section"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn load_reads_package_and_dependencies() {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("coil_manifest_pkg_{pid}_{nanos}"));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("coil.toml"),
            r#"
[package]
name = "spool_consumer"
version = "0.1.0"

[module]
roots = ["./src", "./.spool/deps"]

[dependencies]
http = { git = "https://example.com/http.git", version = "^0.2" }
local_lib = { path = "../local-lib" }
"#,
        )
        .unwrap();

        let m = Manifest::load(&tmp).unwrap();
        assert_eq!(
            m.package,
            Some(PackageInfo {
                name: "spool_consumer".into(),
                version: "0.1.0".into(),
                coil: None,
                include: None,
            })
        );
        assert_eq!(
            m.dependencies,
            vec![
                (
                    "http".into(),
                    DependencySpec::Git {
                        url: "https://example.com/http.git".into(),
                        version: Some("^0.2".into()),
                        rev: None,
                    }
                ),
                (
                    "local_lib".into(),
                    DependencySpec::Path {
                        path: PathBuf::from("../local-lib"),
                    }
                ),
            ]
        );
        assert_eq!(
            m.roots,
            vec![PathBuf::from("./src"), PathBuf::from("./.spool/deps")]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_scripts_section_accepts_all_keys() {
        let src = r#"
            [scripts]
            pre_install = "./scripts/pre-install.sh"
            post_install = "./scripts/post-install.sh"
            pre_update = "./scripts/pre-update.sh"
            post_update = "./scripts/post-update.sh"
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.scripts,
            Scripts {
                pre_install: Some(PathBuf::from("./scripts/pre-install.sh")),
                post_install: Some(PathBuf::from("./scripts/post-install.sh")),
                pre_update: Some(PathBuf::from("./scripts/pre-update.sh")),
                post_update: Some(PathBuf::from("./scripts/post-update.sh")),
            }
        );
    }

    #[test]
    fn parse_scripts_missing_keys_are_none() {
        let src = r#"
            [scripts]
            post_install = "./hooks/after.sh"
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(
            m.scripts.pre_install, None,
            "omitted scripts keys must stay None"
        );
        assert_eq!(
            m.scripts.post_install,
            Some(PathBuf::from("./hooks/after.sh"))
        );
        assert_eq!(m.scripts.pre_update, None);
        assert_eq!(m.scripts.post_update, None);
    }

    #[test]
    fn parse_unknown_scripts_key_errors() {
        let src = "[scripts]\npre_build = \"./nope.sh\"\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(
                    message.contains("unknown key `scripts.pre_build`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_duplicate_scripts_key_errors() {
        let src = "[scripts]\npre_install = \"./a.sh\"\npre_install = \"./b.sh\"\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(
                    message.contains("duplicate key `scripts.pre_install`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_hooks_section_is_unknown() {
        let src = "[hooks]\ninclude = \"./hooks/include.sh\"\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(message.contains("unknown section"), "got {message}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_package_include_accepts_string_path() {
        let src = r#"
            [package]
            name = "native-bits"
            version = "0.1.0"
            include = "./hooks/include.sh"
        "#;
        let m = Manifest::parse(src).unwrap();
        let pkg = m.package.expect("package");
        assert_eq!(pkg.name, "native-bits");
        assert_eq!(pkg.include, Some(PathBuf::from("./hooks/include.sh")));
        assert_eq!(pkg.coil, None);
    }

    #[test]
    fn parse_package_coil_accepts_semver_range() {
        let src = r#"
            [package]
            name = "http"
            version = "0.1.0"
            coil = ">=0.1.0"
        "#;
        let m = Manifest::parse(src).unwrap();
        let pkg = m.package.expect("package");
        assert_eq!(pkg.coil.as_deref(), Some(">=0.1.0"));
        assert_eq!(pkg.include, None);
    }

    #[test]
    fn parse_package_coil_and_include_together() {
        let src = r#"
            [package]
            name = "http"
            version = "0.1.0"
            coil = ">=0.1.0"
            include = "./hooks/include.sh"
        "#;
        let m = Manifest::parse(src).unwrap();
        let pkg = m.package.expect("package");
        assert_eq!(pkg.coil.as_deref(), Some(">=0.1.0"));
        assert_eq!(pkg.include, Some(PathBuf::from("./hooks/include.sh")));
        assert_eq!(m.scripts, Scripts::default());
    }

    #[test]
    fn parse_unknown_package_key_still_errors_with_new_fields() {
        let src = "[package]\nname = \"x\"\nversion = \"0.1.0\"\ncoil = \">=0.1.0\"\nauthors = \"nope\"\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(
                    message.contains("unknown key `package.authors`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_omit_scripts_include_and_coil_still_works() {
        let src = r#"
            [package]
            name = "my_app"
            version = "0.1.0"

            [module]
            roots = ["./src"]
        "#;
        let m = Manifest::parse(src).unwrap();
        let pkg = m.package.expect("package");
        assert_eq!(pkg.name, "my_app");
        assert_eq!(pkg.version, "0.1.0");
        assert_eq!(pkg.coil, None);
        assert_eq!(pkg.include, None);
        assert_eq!(m.scripts, Scripts::default());
    }

    #[test]
    fn parse_package_still_requires_name_and_version_with_coil() {
        let src = "[package]\ncoil = \">=0.1.0\"\n";
        match Manifest::parse(src).unwrap_err() {
            ManifestError::MissingKey { section, key } => {
                assert_eq!(section, "package");
                assert_eq!(key, "name");
            }
            other => panic!("expected MissingKey, got {other:?}"),
        }
    }

    #[test]
    fn parse_duplicate_package_coil_and_include_errors() {
        let dup_coil =
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\ncoil = \">=0.1.0\"\ncoil = \"^0.2\"\n";
        match Manifest::parse(dup_coil).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(
                    message.contains("duplicate key `package.coil`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }

        let dup_include = "[package]\nname = \"a\"\nversion = \"0.1.0\"\ninclude = \"./a.sh\"\ninclude = \"./b.sh\"\n";
        match Manifest::parse(dup_include).unwrap_err() {
            ManifestError::Parse { message, .. } => {
                assert!(
                    message.contains("duplicate key `package.include`"),
                    "got {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }
}
