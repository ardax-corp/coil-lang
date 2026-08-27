//! Fail-closed `dload` gate: consumer allow-list + lock-hashed file.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::resolve::{dload_request_stem, is_libc_alias};
use super::signature::FfiError;

/// Default-deny policy for opening shared libraries.
///
/// The language hardcodes this gate, not a stem list. Allowed stems come from
/// the consumer `coil.toml` `[ffi] allow`; file bytes must match a
/// `[[package.native]]` sha256 in `coil.lock` (or a host [`Self::grant_file`]).
#[derive(Clone, Debug, Default)]
pub struct DloadGate {
    allowed_stems: HashSet<String>,
    hashes_by_stem: HashMap<String, HashSet<[u8; 32]>>,
}

impl DloadGate {
    /// No stems, no hashes — every `dload` is denied.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Build from consumer `[ffi] allow` and lock native `(package, sha256 hex)` pins.
    ///
    /// Libc aliases in `allow` are ignored (they cannot be granted from a manifest).
    /// A lock pin whose package name is not in `allow` is ignored (a dep request
    /// is not a grant). Depending on `tls` only helps after the consumer lists
    /// `tls` and the lock carries that package's hashed native.
    pub fn from_consumer(
        allow: impl IntoIterator<Item = impl AsRef<str>>,
        native_pins: &[(String, String)],
    ) -> Self {
        let mut gate = Self::deny_all();
        for stem in allow {
            let stem = stem.as_ref();
            if is_libc_alias(stem) {
                continue;
            }
            if dload_request_stem(stem) != stem {
                continue;
            }
            gate.allowed_stems.insert(stem.to_string());
        }
        for (pkg, hex) in native_pins {
            if !gate.allowed_stems.contains(pkg.as_str()) {
                continue;
            }
            if let Some(hash) = parse_sha256_hex(hex) {
                gate.hashes_by_stem
                    .entry(pkg.clone())
                    .or_default()
                    .insert(hash);
            }
        }
        gate
    }

    /// Host/test grant: allow `stem` only for files whose contents match `path`.
    pub fn grant_file(&mut self, stem: &str, path: &Path) -> Result<(), FfiError> {
        let hash = sha256_file(path).map_err(|e| FfiError::LibraryDenied {
            name: path.display().to_string(),
            stem: stem.to_string(),
            reason: format!("cannot hash `{}`: {e}", path.display()),
        })?;
        self.allowed_stems.insert(stem.to_string());
        self.hashes_by_stem
            .entry(stem.to_string())
            .or_default()
            .insert(hash);
        Ok(())
    }

    /// Stem check before candidate search. Absolute paths use the filename stem.
    pub fn check_request(&self, name: &str) -> Result<String, FfiError> {
        let stem = dload_request_stem(name);
        if !self.allowed_stems.contains(&stem) {
            return Err(FfiError::LibraryDenied {
                name: name.to_string(),
                stem: stem.clone(),
                reason: if is_libc_alias(name) || is_libc_alias(&stem) {
                    "libc aliases cannot be dloaded".into()
                } else {
                    "stem is not on the consumer [ffi] allow list".into()
                },
            });
        }
        Ok(stem)
    }

    /// Whether `path`'s contents match a lock/host hash for `stem`.
    pub fn file_hash_allowed(&self, stem: &str, path: &Path) -> bool {
        let Some(allowed) = self.hashes_by_stem.get(stem) else {
            return false;
        };
        match sha256_file(path) {
            Ok(hash) => allowed.contains(&hash),
            Err(_) => false,
        }
    }

    /// Denied because the stem is allowed but no pin matched this file.
    pub fn hash_mismatch(name: &str, stem: &str) -> FfiError {
        FfiError::LibraryDenied {
            name: name.to_string(),
            stem: stem.to_string(),
            reason: "shared library is not a lock-hashed native for this stem".into(),
        }
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

pub fn parse_sha256_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = from_hex(chunk[0])?;
        let lo = from_hex(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn deny_all_rejects_c_and_unknown() {
        let g = DloadGate::deny_all();
        assert!(matches!(
            g.check_request("c"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(matches!(
            g.check_request("notalist"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(matches!(
            g.check_request("/lib/x86_64-linux-gnu/libc.so.6"),
            Err(FfiError::LibraryDenied { .. })
        ));
    }

    #[test]
    fn consumer_allow_without_pin_still_fails_hash() {
        let g = DloadGate::from_consumer(["tls"], &[]);
        assert!(g.check_request("tls").is_ok());
        assert!(g.check_request("c").is_err());
        assert!(!g.file_hash_allowed("tls", Path::new("/nope")));
    }

    #[test]
    fn lock_pin_without_allow_is_not_a_grant() {
        let hash = "ab".repeat(32);
        let g = DloadGate::from_consumer(std::iter::empty::<&str>(), &[("tls".into(), hash)]);
        assert!(g.check_request("tls").is_err());
    }

    #[test]
    fn libc_in_allow_is_ignored() {
        let g = DloadGate::from_consumer(["c", "libc", "tls"], &[]);
        assert!(g.check_request("c").is_err());
        assert!(g.check_request("tls").is_ok());
    }

    #[test]
    fn grant_file_allows_matching_bytes_only() {
        let dir = std::env::temp_dir().join("coil_dload_gate_hash");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("libtls.so");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"native-bytes").unwrap();
        drop(f);

        let mut g = DloadGate::deny_all();
        g.grant_file("tls", &path).unwrap();
        assert!(g.check_request("tls").is_ok());
        assert!(g.file_hash_allowed("tls", &path));

        let other = dir.join("other.so");
        std::fs::write(&other, b"different").unwrap();
        assert!(!g.file_hash_allowed("tls", &other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_consumer_binds_pin_to_allowed_stem() {
        let dir = std::env::temp_dir().join("coil_dload_gate_pin");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("libtls.so");
        std::fs::write(&path, b"pinned-tls").unwrap();
        let hex = hex_sha256(&path);

        let g = DloadGate::from_consumer(["tls"], &[("tls".into(), hex)]);
        assert!(g.file_hash_allowed("tls", &path));
        assert!(!g.file_hash_allowed("crypto", &path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn hex_sha256(path: &Path) -> String {
        let h = sha256_file(path).unwrap();
        h.iter().map(|b| format!("{b:02x}")).collect()
    }
}
