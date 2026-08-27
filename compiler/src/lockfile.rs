//! Read `coil.lock` native pins for extra `dload` stems.
//!
//! Spool owns writing this file. The compiler only extracts
//! `[[package.native]] sha256` rows keyed by the enclosing package name,
//! plus `stem`/`lib` so `trusted` deps can skip hash on that extra stem.
//! First-party stems (`crypto`, `tls`, `regex`, `time`) do not need these pins.

use std::path::Path;

use crate::manifest::DependencySpec;

/// Parsed lock subset used by the FFI gate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lockfile {
    pub native_pins: Vec<(String, String)>,
    /// `(package name, dload stem)` from `[[package.native]]` (`stem`/`lib` or coil- strip).
    package_stems: Vec<(String, String)>,
}

impl Lockfile {
    /// Load `project_root/coil.lock`. Missing file is an empty pin set.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join("coil.lock");
        match std::fs::read_to_string(&path) {
            Ok(src) => Self::parse(&src),
            Err(_) => Self::default(),
        }
    }

    /// Parse lock text. Unknown keys are ignored. Invalid native rows are skipped.
    pub fn parse(source: &str) -> Self {
        let mut pins = Vec::new();
        let mut package_stems = Vec::new();
        let mut pkg_name = String::new();
        let mut native_sha = None;
        let mut native_stem = None;
        let mut in_native = false;

        let flush_native = |pkg: &str,
                            sha: &Option<String>,
                            stem: &Option<String>,
                            pins: &mut Vec<(String, String)>,
                            package_stems: &mut Vec<(String, String)>| {
            if pkg.is_empty() {
                return;
            }
            let mapped = dload_stem_for_package(pkg, stem.as_deref());
            if stem.is_some() || sha.is_some() {
                if !mapped.is_empty() {
                    package_stems.push((pkg.to_string(), mapped.clone()));
                }
            }
            if let Some(sha256) = sha {
                if !mapped.is_empty() && sha256.len() == 64 {
                    pins.push((mapped, sha256.clone()));
                }
            }
        };

        for raw in source.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if line == "[[package]]" {
                flush_native(
                    &pkg_name,
                    &native_sha,
                    &native_stem,
                    &mut pins,
                    &mut package_stems,
                );
                native_sha = None;
                native_stem = None;
                in_native = false;
                pkg_name.clear();
                continue;
            }
            if line == "[[package.native]]" {
                flush_native(
                    &pkg_name,
                    &native_sha,
                    &native_stem,
                    &mut pins,
                    &mut package_stems,
                );
                native_sha = None;
                native_stem = None;
                in_native = true;
                continue;
            }
            let Some((key, value)) = parse_kv(line) else {
                continue;
            };
            if key == "name" && !in_native {
                pkg_name = unquote(value);
            } else if key == "sha256" && in_native {
                native_sha = Some(unquote(value));
            } else if (key == "stem" || key == "lib") && in_native {
                native_stem = Some(unquote(value));
            }
        }
        flush_native(
            &pkg_name,
            &native_sha,
            &native_stem,
            &mut pins,
            &mut package_stems,
        );
        Self {
            native_pins: pins,
            package_stems,
        }
    }

    pub fn native_pins(&self) -> &[(String, String)] {
        self.native_pins.as_slice()
    }

    /// Extra `dload` stems whose `[dependencies]` row is `trusted = true`.
    ///
    /// Includes the dep name, `coil-` strip, and lock `[[package.native]]` stem.
    pub fn trusted_extra_stems(&self, dependencies: &[(String, DependencySpec)]) -> Vec<String> {
        trusted_extra_stems(dependencies, self)
    }
}

/// Map a lock package to the `dload` stem. `coil-tls` → `tls` unless
/// `[[package.native]]` sets `stem` / `lib`.
pub(crate) fn dload_stem_for_package(pkg: &str, native_stem: Option<&str>) -> String {
    if let Some(stem) = native_stem {
        if !stem.is_empty() {
            return stem.to_string();
        }
    }
    pkg.strip_prefix("coil-").unwrap_or(pkg).to_string()
}

/// Extra stems that skip native sha256: trusted dep name, coil- strip, lock native stem.
pub(crate) fn trusted_extra_stems(
    dependencies: &[(String, DependencySpec)],
    lock: &Lockfile,
) -> Vec<String> {
    let mut stems = Vec::new();
    for (name, spec) in dependencies {
        if !spec.trusted() {
            continue;
        }
        stems.push(name.clone());
        let stripped = dload_stem_for_package(name, None);
        if stripped != *name {
            stems.push(stripped);
        }
        for (pkg, stem) in &lock.package_stems {
            if pkg == name {
                stems.push(stem.clone());
            }
        }
    }
    stems
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(idx) if !line[..idx].contains('\'') && !line[..idx].contains('"') => &line[..idx],
        _ => line,
    }
}

fn parse_kv(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    Some((k.trim(), v.trim()))
}

fn unquote(value: &str) -> String {
    let t = value.trim();
    if t.len() >= 2 {
        let bytes = t.as_bytes();
        if (bytes[0] == b'\'' && bytes[t.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[t.len() - 1] == b'"')
        {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_native_rows_are_empty() {
        let src = "# spool lockfile v1

[[package]]
name = 'tls'
git = 'https://github.com/ardax-corp/coil-tls'
tag = 'v0.1.0'
rev = 'abc'
content_hash = 'ddd'
";
        assert!(Lockfile::parse(src).native_pins.is_empty());
    }

    #[test]
    fn parses_package_native_sha256() {
        let hash = "ab".repeat(32);
        let src = format!(
            "[[package]]
name = 'tls'
git = 'https://github.com/ardax-corp/coil-tls'
rev = 'abc'
content_hash = 'tree'

[[package.native]]
triple = 'x86_64-linux-gnu'
url = 'https://example.test/libtls.so'
sha256 = '{hash}'
"
        );
        let lock = Lockfile::parse(&src);
        assert_eq!(lock.native_pins, vec![("tls".into(), hash)]);
    }

    #[test]
    fn coil_prefixed_package_pins_stripped_stem() {
        let hash = "cd".repeat(32);
        let src = format!(
            "[[package]]
name = 'coil-tls'
[[package.native]]
sha256 = '{hash}'
"
        );
        let lock = Lockfile::parse(&src);
        assert_eq!(lock.native_pins, vec![("tls".into(), hash)]);
    }

    #[test]
    fn native_stem_overrides_package_name() {
        let hash = "ef".repeat(32);
        let src = format!(
            "[[package]]
name = 'coil-http'
[[package.native]]
stem = 'tls'
sha256 = '{hash}'
"
        );
        let lock = Lockfile::parse(&src);
        assert_eq!(lock.native_pins, vec![("tls".into(), hash)]);
    }

    #[test]
    fn skips_short_hashes() {
        let src = "[[package]]
name = 'tls'
[[package.native]]
sha256 = 'abc'
";
        assert!(Lockfile::parse(src).native_pins.is_empty());
    }

    #[test]
    fn load_missing_file_is_empty() {
        let lock = Lockfile::load(Path::new("/no-such-coil-lock-dir"));
        assert!(lock.native_pins.is_empty());
        assert!(lock.package_stems.is_empty());
    }

    #[test]
    fn native_stem_without_sha_is_recorded() {
        let src = "[[package]]
name = 'coil-http'
[[package.native]]
stem = 'plugin'
";
        let lock = Lockfile::parse(src);
        assert!(lock.native_pins.is_empty());
        assert_eq!(
            lock.package_stems,
            vec![("coil-http".into(), "plugin".into())]
        );
    }

    #[test]
    fn trusted_extra_stems_from_dep_name_and_coil_strip() {
        use crate::manifest::DependencySpec;
        let deps = vec![(
            "coil-plugin".into(),
            DependencySpec::Git {
                url: "https://example.com/plugin.git".into(),
                version: None,
                rev: None,
                trusted: true,
            },
        )];
        let stems = trusted_extra_stems(&deps, &Lockfile::default());
        assert!(stems.contains(&"coil-plugin".into()));
        assert!(stems.contains(&"plugin".into()));
    }

    #[test]
    fn trusted_extra_stems_include_lock_native_stem() {
        use crate::manifest::DependencySpec;
        let deps = vec![(
            "coil-http".into(),
            DependencySpec::Path {
                path: "../http".into(),
                trusted: true,
            },
        )];
        let lock = Lockfile::parse(
            "[[package]]
name = 'coil-http'
[[package.native]]
stem = 'plugin'
",
        );
        let stems = lock.trusted_extra_stems(&deps);
        assert!(stems.contains(&"plugin".into()));
        assert!(stems.contains(&"coil-http".into()));
        assert!(stems.contains(&"http".into()));
    }

    #[test]
    fn untrusted_deps_are_not_trusted_stems() {
        use crate::manifest::DependencySpec;
        let deps = vec![(
            "plugin".into(),
            DependencySpec::Git {
                url: "https://example.com/plugin.git".into(),
                version: None,
                rev: None,
                trusted: false,
            },
        )];
        assert!(trusted_extra_stems(&deps, &Lockfile::default()).is_empty());
    }

    #[test]
    fn load_does_not_write_lockfile() {
        let dir = std::env::temp_dir().join(format!("coil_lock_no_write_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!dir.join("coil.lock").exists());
        let lock = Lockfile::load(&dir);
        assert!(lock.native_pins.is_empty());
        assert!(!dir.join("coil.lock").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trusted_stem_mapping_does_not_drop_native_pins() {
        let hash = "ab".repeat(32);
        let lock = Lockfile::parse(&format!(
            "[[package]]
name = 'plugin'
content_hash = 'tree'
[[package.native]]
sha256 = '{hash}'
"
        ));
        assert_eq!(lock.native_pins, vec![("plugin".into(), hash)]);
    }
}
